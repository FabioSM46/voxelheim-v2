package game

import (
	"fmt"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// LearnedMounts is the character's durable set of learned mounts.
//
// One bit per concrete [vnet.MountKind], with the enum's absent zero deliberately
// carrying no bit. The three colours therefore occupy the low three bits and the
// remaining five bits fail closed when a stored record is validated. A named type
// keeps the encoding and its validation in game, where what a stored value means is
// decided; persist writes the byte without interpreting it.
type LearnedMounts uint8

const allLearnedMounts LearnedMounts = 1<<(uint8(vnet.MountKindBlackHorse)-1) |
	1<<(uint8(vnet.MountKindBrownHorse)-1) |
	1<<(uint8(vnet.MountKindGreyHorse)-1)

// Validate refuses bits for mounts this build does not know. A record is accepted
// whole or refused whole, so an unknown bit is never silently discarded.
func (m LearnedMounts) Validate() error {
	if unknown := m &^ allLearnedMounts; unknown != 0 {
		return fmt.Errorf("game: stored learned mounts contain unknown bits %#02x", uint8(unknown))
	}
	return nil
}

// State returns the complete authoritative set in stable enum order.
func (m LearnedMounts) State() protocol.LearnedMounts {
	mounts := make([]vnet.MountKind, 0, 3)
	for _, kind := range [...]vnet.MountKind{
		vnet.MountKindBlackHorse,
		vnet.MountKindBrownHorse,
		vnet.MountKindGreyHorse,
	} {
		if m&(1<<(uint8(kind)-1)) != 0 {
			mounts = append(mounts, kind)
		}
	}
	return protocol.LearnedMounts{Mounts: mounts}
}

// LearnedMountState returns the complete set this player has learned.
func (p *Player) LearnedMountState() protocol.LearnedMounts {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	return p.learnedMounts.State()
}
