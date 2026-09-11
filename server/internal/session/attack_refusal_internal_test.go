package session

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// A starved swing is answered under Energy and a launcher with no ammunition under Attack,
// and neither carries an anchor: an attack names no voxel.
func TestAnAttackRefusalNamesTheSurfaceThatExplainsIt(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		reason vnet.RefusalReason
		want   protocol.ActionRefused
	}{
		{
			vnet.RefusalReasonNotEnoughEnergy,
			protocol.ActionRefused{Action: vnet.RefusedActionEnergy, Reason: vnet.RefusalReasonNotEnoughEnergy},
		},
		{
			vnet.RefusalReasonNoAmmunition,
			protocol.ActionRefused{Action: vnet.RefusedActionAttack, Reason: vnet.RefusalReasonNoAmmunition},
		},
		{
			vnet.RefusalReasonActionForbiddenWhileMounted,
			protocol.ActionRefused{Action: vnet.RefusedActionAttack, Reason: vnet.RefusalReasonActionForbiddenWhileMounted},
		},
	} {
		if got := attackRefusal(tc.reason); got != tc.want {
			t.Errorf("attackRefusal(%s) = %+v, want %+v", tc.reason, got, tc.want)
		}
	}
}
