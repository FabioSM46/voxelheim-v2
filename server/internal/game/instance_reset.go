package game

import "time"

// A saved run lasts until the server's next midnight.
//
// This file is the whole of that sentence: which midnight, and what happens when it
// arrives. It is deliberately one file rather than a rule spread through instance.go,
// because a reset is two decisions that are easy to state and easy to get subtly wrong —
// **which clock measures the day, and who the reset is allowed to disturb** — and both
// of them are here.
//
// **The clock is the real one, and that is a design decision rather than an
// implementation detail.** The world has a day of its own — game.DayLengthTicks, twenty
// minutes of it, persisted by persist.ClockStore — and it is not this one. A dungeon is
// measured in the player's days: cleared last night, available this morning, and not
// four times before breakfast. Reaching for the in-game clock here would make a dungeon
// resettable three times an hour and would tie the reset to how long the server happened
// to be running, which is the opposite of what a daily lockout is. The two clocks are
// separate on purpose and must stay that way.
//
// **The reset never interrupts anybody.** It is evaluated only where a session is already
// known to be empty — see [InstanceManager.Step] — so a party still inside at midnight
// finishes what it is doing, keeps the boss it has already put down, and loses nothing
// under its feet. The run ends the moment the last of them leaves, and the reset applies
// to the next entry. That is the acceptance criterion in one place rather than a property
// somebody would otherwise have to reconstruct from the order of two conditions.

// nextResetUnix is the first midnight strictly after t, in t's own location.
//
// **Strictly after, which is what makes a run cleared exactly at midnight last a day**
// rather than reset in the same second it was saved. The location is the server's, which
// is what "the server's midnight" means: an operator running in one timezone gets one
// reset per calendar day as their players experience it, and moving the machine moves the
// reset with it.
//
// Built from the calendar rather than by adding twenty-four hours, so a day that is
// twenty-three or twenty-five hours long across a daylight-saving change still ends at
// midnight. time.Date normalises a day past the end of the month, which is what makes the
// last day of a month, of a year and of February in a leap year need no case of their own.
func nextResetUnix(t time.Time) int64 {
	year, month, day := t.Date()
	return time.Date(year, month, day+1, 0, 0, 0, 0, t.Location()).Unix()
}

// resetDueLocked reports whether this session's day is over.
//
// Two conditions and they are both load-bearing. **The session must be saved**, because a
// free copy has no reset — it ends on the empty grace, and a zero expiry would otherwise
// read as "expired in 1970". **Its expiry must have arrived**, at or after the stored
// second rather than strictly after, so a reset stored for midnight happens at midnight.
//
// What is deliberately *not* here is a membership test: the one caller has already
// established that nobody is inside, and repeating the check here would put the rule that
// protects a party in two places while making neither of them the answer. The caller
// holds [InstanceManager.mu].
func (m *InstanceManager) resetDueLocked(s *instanceSession, nowUnix int64) bool {
	return s.state == InstanceSaved && s.expiresUnix != 0 && nowUnix >= s.expiresUnix
}
