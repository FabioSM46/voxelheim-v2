package game

import (
	"math"
	"testing"
)

func TestTheProgressionCurveAtEveryBoundaryShape(t *testing.T) {
	t.Parallel()

	if ExperienceCap != 21750 {
		t.Fatalf("experience cap = %d, want the derived total 21750", ExperienceCap)
	}

	for _, tc := range []struct {
		name   string
		total  uint32
		level  uint16
		into   uint32
		toNext uint32
	}{
		{name: "a new character", total: 0, level: 1, into: 0, toNext: 50},
		{name: "one short of level two", total: 49, level: 1, into: 49, toNext: 50},
		{name: "the level two boundary", total: 50, level: 2, into: 0, toNext: 100},
		{name: "one short of level three", total: 149, level: 2, into: 99, toNext: 100},
		{name: "the level three boundary", total: 150, level: 3, into: 0, toNext: 150},
		{name: "one short of the cap", total: ExperienceCap - 1, level: 29, into: 1449, toNext: 1450},
		{name: "the cap", total: ExperienceCap, level: MaxLevel, into: 0, toNext: 1500},
		{name: "past the cap", total: ExperienceCap + 1, level: MaxLevel, into: 0, toNext: 1500},
		{name: "the largest total", total: math.MaxUint32, level: MaxLevel, into: 0, toNext: 1500},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := levelFor(tc.total); got != tc.level {
				t.Errorf("levelFor(%d) = %d, want %d", tc.total, got, tc.level)
			}
			if got := experienceIntoLevel(tc.total); got != tc.into {
				t.Errorf("experienceIntoLevel(%d) = %d, want %d", tc.total, got, tc.into)
			}
			if got := experienceToNext(tc.level); got != tc.toNext {
				t.Errorf("experienceToNext(%d) = %d, want %d", tc.level, got, tc.toNext)
			}
		})
	}
}

func TestAwardExperienceCrossesLevelsAndSaturatesWithoutOverflow(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name      string
		start     uint32
		amount    uint32
		want      uint32
		leveledUp bool
	}{
		{name: "zero changes nothing", start: 0, amount: 0, want: 0},
		{name: "inside a level", start: 0, amount: 49, want: 49},
		{name: "onto a boundary", start: 49, amount: 1, want: 50, leveledUp: true},
		{name: "short of the next boundary", start: 50, amount: 99, want: 149},
		{name: "across several boundaries", start: 50, amount: 250, want: 300, leveledUp: true},
		{name: "past the cap", start: ExperienceCap - 1, amount: math.MaxUint32, want: ExperienceCap, leveledUp: true},
		{name: "while already capped", start: ExperienceCap, amount: math.MaxUint32, want: ExperienceCap},
		{name: "repairs an impossible over-cap total", start: ExperienceCap + 1, amount: 0, want: ExperienceCap},
	} {
		t.Run(tc.name, func(t *testing.T) {
			player := Player{experience: tc.start}
			if got := player.awardExperienceLocked(tc.amount); got != tc.leveledUp {
				t.Errorf("leveledUp = %t, want %t", got, tc.leveledUp)
			}
			if player.experience != tc.want {
				t.Errorf("experience = %d, want %d", player.experience, tc.want)
			}
		})
	}
}

// The wire carries progress within a level rather than the lifetime total. At the cap
// there is no next level, so the contract's no-special-case representation is a full
// final bar: experience and its denominator are equal and non-zero.
func TestProgressionVitalsAreEncodedAtTheStartMiddleAndCap(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name       string
		total      uint32
		level      uint16
		experience uint32
		toNext     uint32
	}{
		{name: "level one", total: 0, level: 1, experience: 0, toNext: 50},
		{name: "inside level two", total: 75, level: 2, experience: 25, toNext: 100},
		{name: "level cap", total: ExperienceCap, level: MaxLevel, experience: 1500, toNext: 1500},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			player, out := h.join(1, [3]float32{0.5, 64, 0.5})
			h.sim.mu.Lock()
			player.experience = tc.total
			h.sim.mu.Unlock()

			h.step()
			vitals := newestSnapshot(t, out).SelfVitals(nil)
			if vitals == nil {
				t.Fatal("the snapshot carries no self_vitals")
			}
			if got := vitals.Level(); got != tc.level {
				t.Errorf("level = %d, want %d", got, tc.level)
			}
			if got := vitals.Experience(); got != tc.experience {
				t.Errorf("experience = %d, want %d", got, tc.experience)
			}
			if got := vitals.ExperienceToNext(); got != tc.toNext {
				t.Errorf("experience_to_next = %d, want %d", got, tc.toNext)
			}
		})
	}
}
