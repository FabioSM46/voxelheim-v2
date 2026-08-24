package game

import (
	"math"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
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
			startLevel := levelFor(tc.start)
			player := Player{experience: tc.start, health: maxHealthFor(startLevel)}
			if got := player.awardExperienceLocked(tc.amount); got != tc.leveledUp {
				t.Errorf("leveledUp = %t, want %t", got, tc.leveledUp)
			}
			if player.experience != tc.want {
				t.Errorf("experience = %d, want %d", player.experience, tc.want)
			}
			wantHealth := maxHealthFor(startLevel)
			if endLevel := levelFor(tc.want); endLevel > startLevel {
				wantHealth += HealthPerLevel * (endLevel - startLevel)
			}
			if player.health != wantHealth {
				t.Errorf("health = %d, want %d after the crossed levels", player.health, wantHealth)
			}
		})
	}
}

func TestMaximumHealthScalesFivePointsPerLevel(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		level uint16
		want  uint16
	}{
		{level: 1, want: 100},
		{level: 2, want: 105},
		{level: 30, want: 245},
	} {
		if got := maxHealthFor(tc.level); got != tc.want {
			t.Errorf("maxHealthFor(%d) = %d, want %d", tc.level, got, tc.want)
		}
	}
}

func TestALevelUpRaisesCurrentHealthWithTheMaximum(t *testing.T) {
	t.Parallel()

	player := Player{experience: experienceBefore(2) - 1, health: 61}
	if !player.awardExperienceLocked(1) {
		t.Fatal("the boundary did not report a level-up")
	}
	if player.health != 61+HealthPerLevel {
		t.Errorf("health after the level-up = %d, want %d", player.health, 61+HealthPerLevel)
	}
}

func TestOnlyALevelBoundaryResendsTheAppearanceToCurrentViewers(t *testing.T) {
	t.Parallel()

	h := newVitalsHarnessAt(t, DefaultTickRate, dropTerrain{groundTop: 63}, 0)
	subject, subjectOut := h.join(1, [3]float32{0.5, 64, 0.5})
	watcher, watcherOut := h.join(2, [3]float32{1.5, 64, 0.5})
	far, farOut := h.join(3, [3]float32{32.5, 64, 0.5})
	h.step()

	if got := appearanceLevels(t, watcherOut, subject.entityID); !equalLevels(got, []uint16{1}) {
		t.Fatalf("the nearby viewer began with levels %v for the subject, want [1]", got)
	}
	if got := appearanceLevels(t, farOut, subject.entityID); len(got) != 0 {
		t.Fatalf("the out-of-view player was sent subject levels %v", got)
	}

	h.sim.mu.Lock()
	if _, held := subject.described[subject.entityID]; !held {
		t.Fatal("the subject had not cached its own initial appearance")
	}
	if _, held := watcher.described[subject.entityID]; !held {
		t.Fatal("the nearby viewer had not cached the subject's initial appearance")
	}
	if _, held := far.described[subject.entityID]; held {
		t.Fatal("the out-of-view player cached an appearance it was never sent")
	}
	if !h.sim.awardExperienceLocked(subject, ExperiencePerLevelStep) {
		t.Fatal("the award did not report crossing into level two")
	}
	if _, held := subject.described[subject.entityID]; held {
		t.Error("the subject's cached description survived its level-up")
	}
	if _, held := watcher.described[subject.entityID]; held {
		t.Error("the nearby viewer's cached description survived the level-up")
	}
	if _, held := far.described[subject.entityID]; held {
		t.Error("the level-up created a cache entry for an out-of-view player")
	}
	h.sim.mu.Unlock()

	h.step()
	if got := appearanceLevels(t, watcherOut, subject.entityID); !equalLevels(got, []uint16{1, 2}) {
		t.Errorf("the nearby viewer received subject levels %v, want [1 2]", got)
	}
	if got := appearanceLevels(t, subjectOut, subject.entityID); !equalLevels(got, []uint16{1, 2}) {
		t.Errorf("the subject received its own levels %v, want [1 2]", got)
	}
	if got := appearanceLevels(t, farOut, subject.entityID); len(got) != 0 {
		t.Errorf("the out-of-view player received subject levels %v after the level-up", got)
	}
	subjectFaces := appearanceFrames(t, subjectOut, subject.entityID)
	watcherFaces := appearanceFrames(t, watcherOut, subject.entityID)
	if len(subjectFaces) != 2 || len(watcherFaces) != 2 {
		t.Fatalf("the level-up produced %d subject and %d watcher appearance frames, want two each", len(subjectFaces), len(watcherFaces))
	}
	if &subjectFaces[1][0] != &watcherFaces[1][0] {
		t.Error("the level-up encoded separate appearance frames for two viewers in one tick")
	}

	h.sim.mu.Lock()
	if h.sim.awardExperienceLocked(subject, 1) {
		t.Error("an award inside level two reported a level boundary")
	}
	if _, held := watcher.described[subject.entityID]; !held {
		t.Error("an award inside one level invalidated the nearby viewer's description")
	}
	h.sim.mu.Unlock()
	h.step()
	if got := appearanceLevels(t, watcherOut, subject.entityID); !equalLevels(got, []uint16{1, 2}) {
		t.Errorf("an award inside one level resent subject levels %v, want the original [1 2]", got)
	}
}

func appearanceLevels(t *testing.T, out *dropSink, entityID uint64) []uint16 {
	t.Helper()

	var levels []uint16
	for _, frame := range appearanceFrames(t, out, entityID) {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		var table flatbuffers.Table
		if !envelope.Payload(&table) {
			t.Fatal("the appearance payload is absent")
		}
		var appearance vnet.PlayerAppearance
		appearance.Init(table.Bytes, table.Pos)
		levels = append(levels, appearance.Level())
	}
	return levels
}

func appearanceFrames(t *testing.T, out *dropSink, entityID uint64) [][]byte {
	t.Helper()

	var frames [][]byte
	for _, frame := range out.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadPlayerAppearance {
			continue
		}
		var table flatbuffers.Table
		if !envelope.Payload(&table) {
			t.Fatal("the appearance payload is absent")
		}
		var appearance vnet.PlayerAppearance
		appearance.Init(table.Bytes, table.Pos)
		if appearance.EntityId() == entityID {
			frames = append(frames, frame)
		}
	}
	return frames
}

func equalLevels(got, want []uint16) bool {
	if len(got) != len(want) {
		return false
	}
	for i := range got {
		if got[i] != want[i] {
			return false
		}
	}
	return true
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
