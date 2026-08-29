package game

import "fmt"

// The world's clock: two numbers, both advanced by one per authoritative tick.
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
// # The two counters, and why one of them is not enough
//
// tickOfDay is where the world stands in its day and wraps at the end of one; worldTick
// is how many ticks this world has ever run and never wraps. A day phase answers "is it
// night", which is what the sky and the spawn director ask. It cannot answer "how long
// has this world been going", because every value it can hold recurs twenty times a
// minute — so a weather system that drifts across days, or a storm counted in weeks,
// has nothing in it to count.
//
// The pair is one number written two ways, and that is an invariant rather than a
// coincidence: worldTick % DayLengthTicks == tickOfDay, at every instant, because
// advanceClockLocked moves both by one and starts from a state where it holds.
// [Sim.RestoreClock] is the only other way in, and it refuses a pair that disagrees.
//
// # Why it advances in Step and nowhere else
//
// Step is the one place a tick happens, and the loop drives it at a fixed rate whether
// or not anybody is connected. That is what makes an unattended server's night arrive
// on time: nothing about the clock depends on who is watching it.
//
// # The storm's deadline is not a tick at all
//
// nextStormUnix is a wall-clock second, and it is the one value here that is not
// counted in ticks. The Fimbulvetr storm rides a real week (GDD §9), and no tick
// counter can express a week that includes the days a server spent switched off. It is
// kept here because it is world state guarded by the same lock and written to the same
// file; what its value should be is the scheduler's decision, not this file's.

// advanceClockLocked moves the world on by exactly one tick: the absolute count by one,
// and the day by one, wrapping at the end of the day. Called with mu held, from Step and
// from nowhere else.
//
// The wrap is a comparison rather than a modulo, and the difference is not style: a
// modulo would also "repair" a value that had somehow got past the end of the day, and
// repairing that is the one thing this design refuses to do — see [Sim.RestoreClock].
// Starting from a value the invariant allows, one increment can only ever reach
// DayLengthTicks exactly.
//
// **worldTick has no wrap and needs none.** A uint64 at DefaultTickRate exhausts itself
// after some thirty billion years, so the overflow this omits is not a case anybody has
// to reason about — unlike the day, whose end arrives every twenty minutes.
func (s *Sim) advanceClockLocked() {
	s.worldTick++

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

// WorldTick is how many ticks this world has ever run, across every restart it has
// survived. It only ever increases.
//
// [Sim.TickOfDay]'s capture, one number wider. What it is *not* is uptime: a world that
// ran for a day, was switched off for a month and came back reads the same value it
// stopped at, because this counts the world's time and not the process's.
func (s *Sim) WorldTick() uint64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.worldTick
}

// DaysElapsed is how many whole days this world has lived through.
//
// Derived rather than stored, and that is deliberate: a counter of its own would be a
// second thing to advance, to persist and to disagree with the first. It is the
// division the invariant already implies — with tickOfDay as its remainder.
func (s *Sim) DaysElapsed() uint64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.worldTick / DayLengthTicks
}

// NextStorm is when the next storm falls due, as a Unix second. Zero means unscheduled.
//
// Zero is a legal instant in 1970 and is used as "nothing is scheduled" anyway, which is
// safe here for a reason rather than by luck: this deadline is only ever compared
// against the present, and every instant this server will ever see is after it. A
// separate boolean would be a second field to keep in step with the first through a file
// format, for a distinction no caller can act on differently.
func (s *Sim) NextStorm() int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.nextStormUnix
}

// ScheduleStorm records when the next storm falls due. Zero unschedules it.
//
// This file holds the deadline and refuses nothing about it. What a sound value is —
// how far ahead, on which weekday, what happens when one is missed — belongs to the
// scheduler that computes it, and a second opinion here could only ever be a copy of
// that rule kept somewhere it is never read.
func (s *Sim) ScheduleStorm(unix int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.nextStormUnix = unix
}

// Clock captures the whole of the world's clock in one lock.
//
// **The three readers above cannot be composed, and that is what this exists for.**
// Calling TickOfDay and then WorldTick takes the lock twice, and the tick loop is free
// to run between them — so the pair a writer captured that way can break the invariant
// [Sim.RestoreClock] enforces, and the file it wrote would be refused on the next start.
// A clock that is saved every five seconds would hit that eventually, and the failure
// would arrive as a world that lost its day for no visible reason.
//
// The individual readers stay, because a caller that wants one number should not have to
// destructure three; this is the one for anybody writing the clock down.
func (s *Sim) Clock() (tickOfDay uint32, worldTick uint64, nextStormUnix int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.tickOfDay, s.worldTick, s.nextStormUnix
}

// RestoreClock puts a stored clock back, or refuses it whole.
//
// **Two refusals, and both are about a value that cannot be true rather than about a
// file that cannot be read.** A tickOfDay at or beyond [DayLengthTicks] is not a tick of
// any day this build has. A worldTick whose remainder is not that tickOfDay is a pair
// that disagrees with itself, and one of the two is wrong without saying which.
//
// **Neither is repaired.** `% DayLengthTicks` would turn a byte-mangled 4,000,000,000
// into a perfectly ordinary mid-afternoon and destroy the only evidence that anything
// was wrong; deriving one of the pair from the other would do the same to a file that
// had lost half of itself. The caller's answer to an error is the one restoreStructures
// gives — start the world at tick 0, log it, and keep the file.
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
func (s *Sim) RestoreClock(tickOfDay uint32, worldTick uint64) error {
	if tickOfDay >= DayLengthTicks {
		return fmt.Errorf("game: a stored tick of day of %d is not inside a day of %d ticks", tickOfDay, DayLengthTicks)
	}
	if worldTick%DayLengthTicks != uint64(tickOfDay) {
		return fmt.Errorf("game: a stored world tick of %d falls at %d of its day, and the stored tick of day is %d",
			worldTick, worldTick%DayLengthTicks, tickOfDay)
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	s.tickOfDay = tickOfDay
	s.worldTick = worldTick
	return nil
}
