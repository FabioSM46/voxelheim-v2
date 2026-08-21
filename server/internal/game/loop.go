// Package game holds the authoritative simulation: the fixed-rate loop that
// advances the world and, in time, everything that decides what happens in it.
//
// Nothing here reads from or writes to a connection. The loop advances state;
// sessions carry the results. Keeping that boundary is what makes the simulation
// testable without a socket, and what keeps a gameplay rule from accidentally
// depending on who is connected.
package game

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"time"
)

// World parameters the protocol exposes to clients. They live here, next to the
// simulation that owns them, and are announced once in ServerWelcome so a client
// never hardcodes them.
const (
	// ChunkSize moved to the world package when terrain generation landed, as the
	// comment here promised: the package that generates chunks owns their size.

	// DefaultTickRate is the authoritative simulation frequency in hertz.
	DefaultTickRate = 20

	// DefaultViewDistance is the chunk-streaming radius, in chunks.
	DefaultViewDistance = 3

	// DayLengthTicks is one full day-night cycle, in the ticks Step counts.
	//
	// Twenty minutes at DefaultTickRate — 24000 / 20 Hz is 1200 seconds. Long enough
	// that a night is something to plan around, short enough that one sitting sees
	// more than one of them. Of those twenty minutes six are dark (see
	// NightStartTicks) and the fourteen that remain are not: twelve of full daylight,
	// with about a minute of dusk and a minute of dawn either side of the night, where
	// a client ramps its sky. **The server's own boundary is a step and not a curve** —
	// IsNight answers yes or no — so the ramps are the client's arithmetic and no
	// number here describes them.
	//
	// **This is the one duration in the simulation that is not derived from the tick
	// rate, so a non-default -tick-rate really does change how long a day lasts:
	// -tick-rate 40 is a ten-minute day.** DeathDuration, the draugr's timings and the
	// drop lifetime are all stated as durations and converted per server, because
	// three seconds of death has to be three seconds everywhere. A day is stated in
	// ticks because ticks are what crosses the wire: ServerWelcome announces this
	// number and every EntitySnapshot carries a tick_of_day measured against it, so
	// both sides count the same integers with no rounding rule to agree on. Derived
	// from the rate instead, the two boundaries below would be per-server values and
	// IsNight — one pure predicate over one uint32 — could not answer without being
	// handed a simulation. The cost is bounded and visible rather than hidden: the
	// client is told the tick rate in the same welcome, so it can still say what a day
	// is in seconds. A flag for the day length is deliberately out of scope; if one
	// ever lands, the wall-clock choice belongs there and not here.
	DayLengthTicks = 24000

	// NightStartTicks is the tick of the day at which night begins and NightEndTicks
	// the tick at which it ends. Night is the half-open range between them, and
	// [IsNight] is the only place that decides it.
	//
	// Minute twelve to minute eighteen at DefaultTickRate: six minutes of dark against
	// fourteen that are not, which is long enough to be an event and short enough that
	// waiting one out indoors is a choice rather than the only sane play.
	//
	// Both satisfy 0 < NightStartTicks < NightEndTicks <= DayLengthTicks, the ordering
	// schemas/handshake.fbs requires of the welcome that announces them. A night that
	// ran to the end of the day would have NightEndTicks == DayLengthTicks; this one
	// does not, so the day closes in daylight rather than in the dark.
	NightStartTicks = 14400
	NightEndTicks   = 21600
)

// IsNight reports whether a tick of the day falls in the world's night.
//
// **The only place the boundary is decided.** Nothing else in this server compares a
// clock against [NightStartTicks] or [NightEndTicks], and nothing else should: a
// second comparison is a second answer to "is it dark", and the two would disagree the
// first time anybody moved a boundary by one tick. Every consumer — the spawn director
// first, and whatever the GDD's darkness rules add after it — asks this.
//
// Half-open, [NightStartTicks, NightEndTicks), for the reason every range in this
// codebase is: the boundaries then partition the day with no tick counted twice and
// none left out, and the last tick of night is NightEndTicks-1 rather than a value
// somebody has to remember to subtract.
//
// A pure function of one number, so it needs no simulation and no lock. It takes the
// tick of the *day* rather than the absolute tick because the absolute tick is a
// uint64 that outlives every day, and reducing it is arithmetic that happens exactly
// once — in Step, which is where the clock lives.
func IsNight(tickOfDay uint32) bool {
	return tickOfDay >= NightStartTicks && tickOfDay < NightEndTicks
}

// maxCatchUpTicks bounds how far behind the loop may fall before it gives up on
// the missed ticks instead of running them back to back.
//
// Running a burst of late ticks is the worse failure: each one is still late, the
// burst starves whatever caused the stall, and a single long pause turns into a
// visible speed-up. Skipping is honest — the simulation admits it lost time.
const maxCatchUpTicks = 5

// Clock is the loop's only source of time.
//
// It is an interface so that tests drive the loop deterministically: with a fake
// clock, a thousand ticks take no wall-clock time and the assertions are exact
// rather than tolerant.
type Clock interface {
	Now() time.Time

	// SleepUntil blocks until deadline. It returns ctx.Err() if ctx ends first,
	// which is how a shutdown interrupts a tick that has not happened yet.
	SleepUntil(ctx context.Context, deadline time.Time) error
}

// SystemClock is the real clock.
type SystemClock struct{}

// Now reports the current time.
func (SystemClock) Now() time.Time { return time.Now() }

// SleepUntil waits for the deadline or the context, whichever comes first.
func (SystemClock) SleepUntil(ctx context.Context, deadline time.Time) error {
	// Checked before the select, because a select with both cases ready picks one at
	// random: with an expired deadline and a cancelled context the loop would run one
	// more tick about half the time. Same trap as the chunk cache's semaphore.
	if err := ctx.Err(); err != nil {
		return err
	}

	timer := time.NewTimer(time.Until(deadline))
	defer timer.Stop()

	select {
	case <-timer.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// Loop advances the simulation at a fixed rate, independently of how many
// clients are connected: an empty server ticks exactly like a full one, so time
// in the world does not depend on who is watching.
type Loop struct {
	interval time.Duration
	clock    Clock
	log      *slog.Logger
	onTick   func(tick uint64)
}

// NewLoop builds a loop running at tickRate hertz.
func NewLoop(tickRate uint8, clock Clock, log *slog.Logger, onTick func(tick uint64)) (*Loop, error) {
	if tickRate < 1 {
		return nil, fmt.Errorf("game: tick rate must be at least 1, got %d", tickRate)
	}
	if clock == nil {
		return nil, errors.New("game: clock must not be nil")
	}
	if log == nil {
		return nil, errors.New("game: logger must not be nil")
	}
	if onTick == nil {
		return nil, errors.New("game: onTick must not be nil")
	}

	return &Loop{
		interval: time.Second / time.Duration(tickRate),
		clock:    clock,
		log:      log,
		onTick:   onTick,
	}, nil
}

// Interval is the wall-clock duration of one tick.
func (l *Loop) Interval() time.Duration { return l.interval }

// Run ticks until ctx ends, then returns ctx's error. Tick numbers start at 1 and
// never repeat.
func (l *Loop) Run(ctx context.Context) error {
	next := l.clock.Now().Add(l.interval)

	for tick := uint64(1); ; tick++ {
		if err := l.clock.SleepUntil(ctx, next); err != nil {
			l.log.Info("tick loop stopped", "ticks", tick-1, "reason", err.Error())
			return err
		}

		l.onTick(tick)

		// Fixed timestep: the schedule advances by exactly one interval per tick,
		// so a tick that ran long borrows from the next one instead of pushing the
		// whole timeline back. Computing the next deadline from Now() would turn
		// every overrun into permanent drift.
		next = next.Add(l.interval)

		if behind := l.clock.Now().Sub(next); behind > maxCatchUpTicks*l.interval {
			l.log.Warn("simulation fell behind; abandoning missed ticks",
				"behind", behind.String(),
				"skipped_ticks", int64(behind/l.interval),
			)
			next = l.clock.Now().Add(l.interval)
		}
	}
}
