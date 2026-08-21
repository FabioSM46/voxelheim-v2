package game

import "fmt"

// The world's clock: one number, advanced by one per authoritative tick.
//
// # Why it is a counter and not a reading of the wall clock
//
// The tick is the simulation's only clock — every other duration in this package is
// converted into ticks for exactly that reason — and time of day is no different. A
// server that asked time.Now() would run its day at the speed of the machine rather
// than at the speed of the world it is simulating, and a stall that made the loop
// abandon missed ticks (see maxCatchUpTicks) would silently skip that much daylight
// while the players saw nothing move.
//
// # Why it advances in Step and nowhere else
//
// Step is the one place a tick happens, and the loop drives it at a fixed rate whether
// or not anybody is connected. That is what makes an unattended server's night arrive
// on time: nothing about the clock depends on who is watching it.
//
// # Nothing reads it to decide anything, yet
//
// It is announced in every snapshot and it is what [IsNight] is asked about. The first
// consumer of that predicate is the spawn director, which is its own issue — so if you
// are here to make a mob, a light level or a temperature depend on the time, this is
// the file to read and the wrong one to edit.

// advanceClockLocked moves the world's day on by exactly one tick, wrapping at the end
// of the day. Called with mu held, from Step and from nowhere else.
//
// The wrap is a comparison rather than a modulo, and the difference is not style: a
// modulo would also "repair" a value that had somehow got past the end of the day, and
// repairing that is the one thing this design refuses to do — see [Sim.RestoreClock].
// Starting from a value the invariant allows, one increment can only ever reach
// DayLengthTicks exactly.
func (s *Sim) advanceClockLocked() {
	s.tickOfDay++
	if s.tickOfDay >= DayLengthTicks {
		s.tickOfDay = 0
	}
}

// TickOfDay is where the world stands in its day: a value in 0..DayLengthTicks-1.
//
// The capture half of the capture-and-write split this server keeps everywhere it
// touches a disk: the lock is taken, one number is copied, the lock is released, and
// the caller writes with nothing held.
func (s *Sim) TickOfDay() uint32 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.tickOfDay
}

// RestoreClock puts a stored time of day back, or refuses it whole.
//
// **A value at or beyond [DayLengthTicks] is refused rather than wrapped**, and that is
// the whole of what this function decides. A stored tick that cannot be right is not
// quietly made plausible: `% DayLengthTicks` would turn a byte-mangled 4,000,000,000
// into a perfectly ordinary mid-afternoon and destroy the only evidence that anything
// was wrong. The caller's answer to an error is the one restoreStructures gives — start
// the world at tick 0, log it, and keep the file.
//
// **The range check lives here and not in internal/persist**, and the layering is the
// reason. That package writes bytes down and judges only what a *file* can be wrong
// about — magic, version, checksum, size; what those bytes are allowed to *mean* is
// decided by the package holding the constants, exactly as [Life.Validate] decides what
// a stored life may say. persist does not import game and must not start.
//
// **Startup only, before the listener is served.** Nothing enforces that, because
// unlike [Sim.RestoreStructures] — which would silently drop a camp if it ran twice —
// a second call here is a visible jump in the sky rather than a silent loss, and a flag
// to forbid it would be state kept for a caller that does not exist.
func (s *Sim) RestoreClock(tickOfDay uint32) error {
	if tickOfDay >= DayLengthTicks {
		return fmt.Errorf("game: a stored tick of day of %d is not inside a day of %d ticks", tickOfDay, DayLengthTicks)
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	s.tickOfDay = tickOfDay
	return nil
}
