package world

import "testing"

// The leash zones, read against the placed anchors at every rotation: every slot
// stands in its own group's zone, and no zone reaches a checkpoint or a door.

func TestEveryMinorSlotStandsInItsGroupsZone(t *testing.T) {
	t.Parallel()
	for seed := int64(0); seed < 4; seed++ {
		zones := InstanceDungeonZones(seed)
		if len(zones) != SandBuriedGroup+1 {
			t.Fatalf("seed %d: %d zones, want one per group", seed, len(zones))
		}
		slots := 0
		for _, a := range InstanceDungeonAnchors(seed) {
			if a.Kind != AnchorInstanceMinorSpawn {
				continue
			}
			slots++
			if a.Index < 0 || a.Index >= len(zones) || zones[a.Index].Group != a.Index {
				t.Fatalf("seed %d: slot %+v has no zone", seed, a)
			}
			if !zones[a.Index].Contains(a.X, a.Y, a.Z) {
				t.Errorf("seed %d: slot %+v stands outside its zone %+v", seed, a, zones[a.Index])
			}
		}
		if slots != 20+6+5 {
			t.Errorf("seed %d: %d minor slots", seed, slots)
		}
	}
}

func TestEveryDungeonZoneEndsAtEachCheckpointAndDoor(t *testing.T) {
	t.Parallel()
	for seed := int64(0); seed < 4; seed++ {
		zones := InstanceDungeonZones(seed)
		for _, a := range InstanceDungeonAnchors(seed) {
			if a.Kind != AnchorInstanceCheckpoint && a.Kind != AnchorInstanceDoor {
				continue
			}
			for _, z := range zones {
				if z.Contains(a.X, a.Y, a.Z) {
					t.Errorf("seed %d: zone %d reaches %v %+v", seed, z.Group, a.Kind, a)
				}
			}
		}
	}
}

// A trigger lies inside the zone of the creatures it wakes, so whoever fires it is
// already prey for them.
func TestEachTriggerLiesInsideItsZone(t *testing.T) {
	t.Parallel()
	wakes := map[int]int{CaveTrigger: CaveBurrowGroup, SandTrigger: SandBuriedGroup}
	for seed := int64(0); seed < 4; seed++ {
		zones := InstanceDungeonZones(seed)
		for _, a := range InstanceDungeonAnchors(seed) {
			if a.Kind != AnchorInstanceTrigger {
				continue
			}
			if z := zones[wakes[a.Index]]; !z.Contains(a.X, a.Y, a.Z) {
				t.Errorf("seed %d: trigger corner %+v lies outside zone %+v", seed, a, z)
			}
		}
	}
}

// The spider waves start on whichever the layout reaches first, the cavern's trigger
// or the web curtain. From the shore the tunnel opens into the cavern and the curtain
// is across the neck beyond it, so the trigger is always first: nothing else needs to
// watch the curtain.
func TestTheCaveTriggerComesBeforeTheWebCurtain(t *testing.T) {
	t.Parallel()
	tunnelMouth := caveCavernZ1 + 1 // the tunnel from the shore runs south of the cavern
	if caveCurtainZ >= caveCavernZ0 || caveCavernZ1 >= tunnelMouth {
		t.Fatalf("the curtain at z %d is not beyond the cavern %d..%d from the tunnel at %d",
			caveCurtainZ, caveCavernZ0, caveCavernZ1, tunnelMouth)
	}
	if instanceDungeon.At(17, dungeonShore, caveCurtainZ) != Cobweb {
		t.Fatal("the neck holds no curtain where the drawing says")
	}
}
