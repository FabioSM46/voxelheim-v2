package game

import (
	"math"
	"testing"
)

// The world's clock, driven through the harness the drop tests already use: it steps a
// real Sim one tick at a time exactly as the loop does, so what these assert is what a
// running server does rather than what a hand-called helper would.

// A world nobody has restored anything into begins at the first tick of the day. Zero
// is the value main's absent-file path relies on, so it is pinned here rather than left
// to the zero value of a field somebody could later initialise.
func TestAFreshWorldStartsAtFirstLight(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})

	if got := h.sim.TickOfDay(); got != 0 {
		t.Errorf("a fresh world starts at tick %d of the day, want 0", got)
	}
	if IsNight(h.sim.TickOfDay()) {
		t.Error("a fresh world starts at night")
	}
}

// One per tick, exactly — the whole of the acceptance criterion, checked after every
// step rather than only at the end so an advance of two that later corrected itself
// could not pass.
func TestTheClockAdvancesOnePerTick(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	h.join(1, [3]float32{0.5, 64, 0.5})

	for want := uint32(1); want <= 200; want++ {
		h.step()
		if got := h.sim.TickOfDay(); got != want {
			t.Fatalf("after %d ticks the clock reads %d", want, got)
		}
	}
}

// The claim the fixed-rate loop exists for, at the level of the clock: time in the world
// does not depend on who is watching it, so an unattended server's night arrives on
// time. Same simulation, nobody joined.
func TestTheClockAdvancesWithNobodyConnected(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	h.advance(50)

	if got := h.sim.TickOfDay(); got != 50 {
		t.Errorf("an empty server's clock reads %d after 50 ticks, want 50", got)
	}
}

// The end of the day is a wrap to zero and not a value that keeps climbing, and the
// tick after it is 1 rather than a second 0 — which is the shape a wrap written as
// "reset when past" gets wrong.
func TestTheClockWrapsAtTheEndOfTheDay(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	if err := h.sim.RestoreClock(DayLengthTicks - 2); err != nil {
		t.Fatalf("RestoreClock: %v", err)
	}

	for i, want := range []uint32{DayLengthTicks - 1, 0, 1} {
		h.step()
		if got := h.sim.TickOfDay(); got != want {
			t.Fatalf("tick %d across the wrap reads %d, want %d", i+1, got, want)
		}
	}
}

// IsNight is the only place the boundary is decided, so this is where the boundary is
// pinned. Both edges, from both sides: the half-open range means the tick at
// NightStartTicks is the first dark one and the tick at NightEndTicks is the first that
// is not.
func TestItIsNightExactlyBetweenTheTwoBoundaries(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		tickOfDay uint32
		night     bool
		what      string
	}{
		{0, false, "the first tick of the day"},
		{NightStartTicks - 1, false, "the last tick before dusk"},
		{NightStartTicks, true, "the first tick of night"},
		{NightStartTicks + 1, true, "just after dusk"},
		{NightEndTicks - 1, true, "the last tick of night"},
		{NightEndTicks, false, "the first tick after dawn"},
		{DayLengthTicks - 1, false, "the last tick of the day"},
	} {
		if got := IsNight(tc.tickOfDay); got != tc.night {
			t.Errorf("IsNight(%d) = %v at %s, want %v", tc.tickOfDay, got, tc.what, tc.night)
		}
	}
}

// The same predicate counted rather than sampled, so a boundary moved by one tick fails
// here even if somebody moved the table above with it.
func TestNightIsAsLongAsTheBoundariesSay(t *testing.T) {
	t.Parallel()

	dark := 0
	for tickOfDay := uint32(0); tickOfDay < DayLengthTicks; tickOfDay++ {
		if IsNight(tickOfDay) {
			dark++
		}
	}

	if want := NightEndTicks - NightStartTicks; dark != want {
		t.Errorf("%d ticks of the day are night, want %d", dark, want)
	}
}

// The invariant schemas/handshake.fbs requires of the welcome that announces these
// three, executed on this side. The client refuses a welcome that breaks it, so a
// server that could send one would be a server nobody could connect to — and this is
// the test that fails instead of every session.
func TestTheAnnouncedDayIsInternallyConsistent(t *testing.T) {
	t.Parallel()

	// One clause at a time, so a failure names which half of the ordering broke rather
	// than only that the whole of it did.
	for _, rule := range []struct {
		holds bool
		what  string
	}{
		// Zero is the wire's "this server keeps no clock", so a day length of zero
		// would announce the absence of the thing this package now owns.
		{DayLengthTicks > 0, "the day has a length at all"},
		{NightStartTicks > 0, "night begins after the first tick of the day"},
		{NightStartTicks < NightEndTicks, "night begins before it ends"},
		{NightEndTicks <= DayLengthTicks, "night ends no later than the day does"},
	} {
		if !rule.holds {
			t.Errorf("night %d..%d in a day of %d ticks breaks: %s",
				NightStartTicks, NightEndTicks, DayLengthTicks, rule.what)
		}
	}
}

// The arithmetic the constants' comment states, pinned so the prose and the numbers
// cannot drift apart. Twenty minutes at the default rate, six of them dark.
func TestTheDayIsTwentyMinutesAtTheDefaultTickRate(t *testing.T) {
	t.Parallel()

	if seconds := DayLengthTicks / DefaultTickRate; seconds != 20*60 {
		t.Errorf("a day is %d seconds at %d Hz, want %d", seconds, DefaultTickRate, 20*60)
	}
	if seconds := (NightEndTicks - NightStartTicks) / DefaultTickRate; seconds != 6*60 {
		t.Errorf("night is %d seconds at %d Hz, want %d", seconds, DefaultTickRate, 6*60)
	}
}

// A tick that cannot be right is refused rather than wrapped, and the clock it was
// offered to is left exactly where it was. Wrapping would turn a mangled value into a
// plausible one and destroy the only evidence that anything was wrong.
func TestRestoreClockRefusesATickOutsideTheDay(t *testing.T) {
	t.Parallel()

	for _, tickOfDay := range []uint32{DayLengthTicks, DayLengthTicks + 1, 2 * DayLengthTicks, math.MaxUint32} {
		h := newDropHarness(t, dropTerrain{groundTop: 63})
		h.advance(7)

		if err := h.sim.RestoreClock(tickOfDay); err == nil {
			t.Errorf("RestoreClock(%d) was accepted into a day of %d ticks", tickOfDay, DayLengthTicks)
		}
		if got := h.sim.TickOfDay(); got != 7 {
			t.Errorf("a refused RestoreClock(%d) moved the clock to %d, want it left at 7", tickOfDay, got)
		}
	}
}

// The other side of the same boundary: every tick a day contains is one this build will
// take back, including the last one — which is the value the wrap test starts from and
// the one an off-by-one in the check would refuse.
func TestRestoreClockAcceptsEveryTickInsideTheDay(t *testing.T) {
	t.Parallel()

	for _, tickOfDay := range []uint32{0, 1, NightStartTicks, NightEndTicks, DayLengthTicks - 1} {
		h := newDropHarness(t, dropTerrain{groundTop: 63})

		if err := h.sim.RestoreClock(tickOfDay); err != nil {
			t.Errorf("RestoreClock(%d): %v", tickOfDay, err)
			continue
		}
		if got := h.sim.TickOfDay(); got != tickOfDay {
			t.Errorf("RestoreClock(%d) left the clock at %d", tickOfDay, got)
		}
	}
}

// The wire half: tick_of_day rides every snapshot, carrying the value the tick that
// built it advanced to — not the one before it, and not the absolute tick.
func TestEverySnapshotCarriesTheTickOfDay(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})

	if err := h.sim.RestoreClock(NightStartTicks - 1); err != nil {
		t.Fatalf("RestoreClock: %v", err)
	}

	for i, want := range []uint32{NightStartTicks, NightStartTicks + 1, NightStartTicks + 2} {
		h.step()
		if got := newestSnapshot(t, out).TickOfDay(); got != want {
			t.Errorf("snapshot %d carries tick_of_day %d, want %d", i+1, got, want)
		}
	}

	// And it is the simulation's own clock rather than a number the encoder invented.
	if got, want := newestSnapshot(t, out).TickOfDay(), h.sim.TickOfDay(); got != want {
		t.Errorf("the snapshot says %d and the simulation says %d", got, want)
	}
}

// The contract's bound on that field, executed against the frames the simulation
// actually emits: tick_of_day < day_length_ticks, on every tick of a whole day. The
// client refuses a snapshot that breaks it, so this is the test that fails instead of
// the connection — and it is the one that would catch a wrap written as `>` .
func TestTheTickOfDayTheTickEmitsIsAlwaysInsideTheDay(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})

	// Two ticks short of midnight, so the run crosses the wrap rather than stopping at
	// it, without stepping a whole simulated day for a check about one number.
	if err := h.sim.RestoreClock(DayLengthTicks - 3); err != nil {
		t.Fatalf("RestoreClock: %v", err)
	}
	for range 6 {
		h.step()
		if got := newestSnapshot(t, out).TickOfDay(); got >= DayLengthTicks {
			t.Fatalf("a snapshot carries tick_of_day %d, which is not inside a %d-tick day", got, DayLengthTicks)
		}
	}
}
