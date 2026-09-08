package game

import (
	"math"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// A physical combination is bounded at three independently announced blows. It is
// not a channel: every blow gets a fresh instance and hit ledger. The sequence is
// owned by its running instance, so withdrawing that instance cancels the future too.
// PrisonerClaws can opt into this same catalogue seam when its alternating set lands.
type encounterCombo struct {
	total          uint8
	between, final time.Duration
	blows          [3]encounterComboBlow
}

type encounterComboBlow struct {
	hazard  encounterHazard
	bearing float64
}

var biteCombo = encounterCombo{
	total: 2, between: 400 * time.Millisecond, final: 1800 * time.Millisecond,
	blows: [3]encounterComboBlow{
		{hazard: encounterHazard{shape: vnet.HazardShapeCone, reach: 3, height: 2.2, halfAngle: .70}},
		{hazard: encounterHazard{shape: vnet.HazardShapeCone, reach: 3, height: 2.2, halfAngle: .70}},
	},
}

var tollCombo = encounterCombo{
	total: 3, between: 400 * time.Millisecond, final: 2200 * time.Millisecond,
	blows: [3]encounterComboBlow{
		{bearing: -.45, hazard: encounterHazard{shape: vnet.HazardShapeCone, reach: 3.8, height: 3, halfAngle: .95}},
		{bearing: .45, hazard: encounterHazard{shape: vnet.HazardShapeCone, reach: 3.8, height: 3, halfAngle: .95}},
		{hazard: encounterHazard{shape: vnet.HazardShapeLine, reach: 3.8, height: 3, halfWidth: .65}},
	},
}

// forComboStep is used before both the escape check and the commitment. Replacing
// rather than accumulating the bearing means the second blow cannot inherit the
// first blow's turn. Its aim is then locked once for the whole instance.
func (d encounterMoveDef) forComboStep(step uint8) encounterMoveDef {
	if d.combo != nil && step > 0 && step <= d.combo.total {
		blow := d.combo.blows[step-1]
		d.hazard, d.bearing = blow.hazard, blow.bearing
	}
	return d
}

func (m *mob) aimForMove(def encounterMoveDef, target *Player) [3]float64 {
	aim := m.aimAt(target)
	sine, cosine := math.Sincos(def.bearing)
	return [3]float64{aim[0]*cosine - aim[2]*sine, 0, aim[0]*sine + aim[2]*cosine}
}

// continueComboLocked runs only after a safe inter-blow recovery. It commits the
// next telegraph only if the current target can still read and escape that exact
// next shape. If not, the previous instance keeps its immutable position and pays
// a full final opening; it cannot retry the continuation after that opening.
func (m *mob) continueComboLocked(s *Sim, players []*Player, tick uint64) bool {
	r := m.encounter.running
	if r.comboStep == 0 || r.comboStep == r.comboTotal || r.comboStopped {
		return false
	}
	next := r.comboStep + 1
	def := r.def.forComboStep(next)
	target := m.chooseTargetLocked(s, players)
	valid := false
	if target != nil {
		body := m.species().body.boxAt(m.pos)
		distance := boxDistance(body, target.box())
		valid = distance >= def.minRange && distance <= def.maxRange &&
			clearLineOfSight(s.terrain, boxCentre(body), boxCentre(target.box())) &&
			s.moveLeavesAnEscapeLocked(m, def, target)
	}
	if valid {
		m.finishEncounterMoveLocked(vnet.MoveEndCompleted)
		m.beginEncounterComboBlowLocked(s, def, target, tick, next)
	} else {
		r.comboStopped = true
		m.enterMovePhaseLocked(vnet.MovePhaseRecovery, r.ticks.comboFinal, tick)
		r.remaining-- // this tick spends the newly published safe opening
		m.action = vnet.MobActionRecovery
		m.vel[0], m.vel[2] = 0, 0
		m.publishRunningMoveLocked()
	}
	return true
}
