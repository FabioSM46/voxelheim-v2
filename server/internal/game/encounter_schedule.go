package game

import vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"

// A slot prefers eligible moves; it never bypasses range, cooldown, LOS or escape.
// Repeated duel slots provide openings between spells; LRU breaks class ties.
type kingScheduleSlot []vnet.EncounterMoveKind

var (
	kingDuel      = kingScheduleSlot{vnet.EncounterMoveKindKingsSentence, vnet.EncounterMoveKindThreeTolls}
	kingSpear     = kingScheduleSlot{vnet.EncounterMoveKindSepulchreSpear}
	kingRitual    = kingScheduleSlot{vnet.EncounterMoveKindBurial, vnet.EncounterMoveKindEdictOfTheGraves}
	kingSchedules = [][]kingScheduleSlot{
		{kingDuel, kingSpear},
		{kingRitual, kingDuel, kingSpear, kingDuel},
		{{vnet.EncounterMoveKindBurial}, {vnet.EncounterMoveKindKingsSentence}, {vnet.EncounterMoveKindEdictOfTheGraves}, kingSpear, {vnet.EncounterMoveKindRequiemOfTheBuried}, {vnet.EncounterMoveKindThreeTolls}},
	}
)

func (m *mob) kingSchedule() ([]kingScheduleSlot, int) {
	if m.kind != vnet.MobKindDraugrKing || m.encounter == nil || m.encounter.phase == 0 {
		return nil, 0
	}
	e := m.encounter
	slots := kingSchedules[min(int(e.phase), len(kingSchedules))-1]
	cursor := 0
	if e.scheduleStage == e.phase {
		cursor = int(e.scheduleCursor) % len(slots)
	}
	return slots, cursor
}

// Scan at most six slots. Sorting candidates by distance skips unavailable slots
// without a retry loop or a compulsory cast. Escape is checked once per candidate.
func (m *mob) schedulePriority(kind vnet.EncounterMoveKind) int {
	slots, cursor := m.kingSchedule()
	for offset := range len(slots) {
		for _, member := range slots[(cursor+offset)%len(slots)] {
			if member == kind {
				return offset
			}
		}
	}
	return len(slots)
}

// Only a committed root advances the cycle. Stage changes affect the next root
// after the current move's complete recovery, never its telegraph/pulses/combo.
// This cursor is a preference, not a queue, and dies with the encounter.
func (m *mob) commitSchedule(kind vnet.EncounterMoveKind) {
	slots, cursor := m.kingSchedule()
	if len(slots) == 0 {
		return
	}
	offset := m.schedulePriority(kind)
	if offset == len(slots) {
		return
	}
	m.encounter.scheduleStage = m.encounter.phase
	m.encounter.scheduleCursor = uint8((cursor + offset + 1) % len(slots))
}
