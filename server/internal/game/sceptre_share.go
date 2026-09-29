package game

import vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"

// OrbBladeShare is what one sceptre is worth, in iron blades, against a creature of kind:
// the orb's damage over the blade's, each after the species' armour. Both weapons spend
// [AttackEnergyCost] of the same reserve, so they land at the same pace and the share of
// the damage is the share of the kill.
//
// It decides nothing in the simulation. It is the one number the healer route estimate
// (dungeon_route_healer_estimate_test.go) and the descent bot's report both read, kept
// here so that the estimate that is printed is the one that is validated (#1370).
func OrbBladeShare(kind vnet.MobKind) float64 {
	m := &mob{kind: kind}
	return float64(m.armoured(OrbDamage)) / float64(m.armoured(IronSwordDamage))
}
