package game_test

import (
	"context"
	"errors"
	"log/slog"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
)

// fakeClock runs simulated time. SleepUntil jumps straight to the deadline, so a
// test covering a minute of ticks costs nothing and its assertions are exact.
type fakeClock struct {
	now       time.Time
	deadlines []time.Time
	// advancePerTick simulates work inside a tick: time that passes without the
	// loop asking for it.
	advancePerTick time.Duration
	jumpAtTick     int
	jump           time.Duration
	ticks          int
}

func newFakeClock() *fakeClock {
	return &fakeClock{now: time.Date(2026, 8, 17, 12, 0, 0, 0, time.UTC)}
}

func (f *fakeClock) Now() time.Time { return f.now }

func (f *fakeClock) SleepUntil(ctx context.Context, deadline time.Time) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	f.deadlines = append(f.deadlines, deadline)
	if deadline.After(f.now) {
		f.now = deadline
	}
	return nil
}

// tock is called from onTick to model time spent inside the tick itself.
func (f *fakeClock) tock() {
	f.ticks++
	f.now = f.now.Add(f.advancePerTick)
	if f.jumpAtTick != 0 && f.ticks == f.jumpAtTick {
		f.now = f.now.Add(f.jump)
	}
}

func discard() *slog.Logger { return slog.New(slog.DiscardHandler) }

func TestLoopRunsExactlyTheExpectedTicks(t *testing.T) {
	t.Parallel()

	const want = 40
	clock := newFakeClock()
	start := clock.Now()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	var seen []uint64
	loop, err := game.NewLoop(20, clock, discard(), func(tick uint64) {
		clock.tock()
		seen = append(seen, tick)
		if len(seen) == want {
			cancel()
		}
	})
	if err != nil {
		t.Fatalf("NewLoop: %v", err)
	}

	if err := loop.Run(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("Run returned %v, want context.Canceled", err)
	}

	if len(seen) != want {
		t.Fatalf("ran %d ticks, want %d", len(seen), want)
	}
	for i, tick := range seen {
		if tick != uint64(i)+1 {
			t.Fatalf("tick %d of the sequence was numbered %d", i+1, tick)
		}
	}

	// 40 ticks at 20 Hz is exactly two seconds of simulated time.
	if got := clock.deadlines[len(clock.deadlines)-1].Sub(start); got != want*loop.Interval() {
		t.Errorf("last deadline was %v after the start, want %v", got, want*loop.Interval())
	}
}

// The fixed timestep is the property worth pinning: deadlines must stay exactly
// one interval apart even when each tick consumes most of its budget. Deriving the
// next deadline from Now() would pass a "did it tick?" test and still drift.
func TestLoopDoesNotDriftWhenTicksRunLong(t *testing.T) {
	t.Parallel()

	clock := newFakeClock()
	clock.advancePerTick = 40 * time.Millisecond // 80% of a 20 Hz budget

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	count := 0
	loop, err := game.NewLoop(20, clock, discard(), func(uint64) {
		clock.tock()
		count++
		if count == 25 {
			cancel()
		}
	})
	if err != nil {
		t.Fatalf("NewLoop: %v", err)
	}
	_ = loop.Run(ctx)

	if len(clock.deadlines) < 25 {
		t.Fatalf("only %d deadlines recorded", len(clock.deadlines))
	}
	for i := 1; i < 25; i++ {
		if gap := clock.deadlines[i].Sub(clock.deadlines[i-1]); gap != loop.Interval() {
			t.Fatalf("deadline %d came %v after the previous one, want %v", i, gap, loop.Interval())
		}
	}
}

func TestLoopAbandonsMissedTicksAfterAStall(t *testing.T) {
	t.Parallel()

	clock := newFakeClock()
	clock.jumpAtTick = 3
	clock.jump = 30 * (50 * time.Millisecond) // 30 ticks' worth of stall at 20 Hz

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	count := 0
	loop, err := game.NewLoop(20, clock, discard(), func(uint64) {
		clock.tock()
		count++
		if count == 8 {
			cancel()
		}
	})
	if err != nil {
		t.Fatalf("NewLoop: %v", err)
	}
	_ = loop.Run(ctx)

	// The deadline after the stall must be one interval past the *new* now, not a
	// backlog of 30 deadlines already in the past.
	gapAcrossStall := clock.deadlines[3].Sub(clock.deadlines[2])
	if gapAcrossStall <= loop.Interval() {
		t.Fatalf("deadline did not jump across the stall: gap %v", gapAcrossStall)
	}
	for i := 5; i < 8; i++ {
		if gap := clock.deadlines[i].Sub(clock.deadlines[i-1]); gap != loop.Interval() {
			t.Errorf("after the stall, deadline %d came %v after the previous one, want %v", i, gap, loop.Interval())
		}
	}
}

func TestLoopStopsBeforeTheFirstTickWhenAlreadyCancelled(t *testing.T) {
	t.Parallel()

	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	ticked := false
	loop, err := game.NewLoop(20, newFakeClock(), discard(), func(uint64) { ticked = true })
	if err != nil {
		t.Fatalf("NewLoop: %v", err)
	}
	if err := loop.Run(ctx); err == nil {
		t.Fatal("Run returned nil for a cancelled context")
	}
	if ticked {
		t.Error("the loop ticked despite a cancelled context")
	}
}

func TestNewLoopRejectsBadArguments(t *testing.T) {
	t.Parallel()

	if _, err := game.NewLoop(0, newFakeClock(), discard(), func(uint64) {}); err == nil {
		t.Error("a tick rate of 0 was accepted")
	}
	if _, err := game.NewLoop(20, newFakeClock(), discard(), nil); err == nil {
		t.Error("a nil onTick was accepted")
	}
	if _, err := game.NewLoop(20, nil, discard(), func(uint64) {}); err == nil {
		t.Error("a nil clock was accepted")
	}
	if _, err := game.NewLoop(20, newFakeClock(), nil, func(uint64) {}); err == nil {
		t.Error("a nil logger was accepted")
	}
}

func TestSystemClockSleepUntilHonoursContext(t *testing.T) {
	t.Parallel()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Millisecond)
	defer cancel()

	start := time.Now()
	err := game.SystemClock{}.SleepUntil(ctx, start.Add(10*time.Second))
	if err == nil {
		t.Fatal("SleepUntil ignored the context")
	}
	if elapsed := time.Since(start); elapsed > time.Second {
		t.Errorf("SleepUntil took %v to notice a cancelled context", elapsed)
	}
}
