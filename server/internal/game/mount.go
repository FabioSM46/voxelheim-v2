package game

import (
	"errors"
	"fmt"
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// ErrActionForbiddenWhileMounted is the stable identity session routes use when
// an older silent action path now owes the mounted refusal the V27 contract names.
var ErrActionForbiddenWhileMounted = errors.New("the action is forbidden while mounted")

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

// learnedMountBit is the one translation between the wire enum and the stored set.
// Unknown and future values fail closed rather than shifting by an invalid amount or
// silently allocating a bit this build cannot validate on the next restart.
func learnedMountBit(kind vnet.MountKind) (LearnedMounts, bool) {
	if kind < vnet.MountKindBlackHorse || kind > vnet.MountKindGreyHorse {
		return 0, false
	}
	return 1 << (uint8(kind) - 1), true
}

// Validate refuses bits for mounts this build does not know. A record is accepted
// whole or refused whole, so an unknown bit is never silently discarded.
func (m LearnedMounts) Validate() error {
	if unknown := m &^ allLearnedMounts; unknown != 0 {
		return fmt.Errorf("game: stored learned mounts contain unknown bits %#02x", uint8(unknown))
	}
	return nil
}

// Has reports whether this set already contains one known concrete mount.
func (m LearnedMounts) Has(kind vnet.MountKind) bool {
	bit, known := learnedMountBit(kind)
	return known && m&bit != 0
}

// Learn returns the set with one known concrete mount added and whether it was new.
// The registry only names known mounts, but the bool keeps a future bad row fail-closed.
func (m LearnedMounts) Learn(kind vnet.MountKind) (LearnedMounts, bool) {
	bit, known := learnedMountBit(kind)
	if !known || m&bit != 0 {
		return m, false
	}
	return m | bit, true
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

// Mount admits one learned horse into the common authoritative cast. Completion
// rechecks the world: a roof may be placed or the ground may disappear during the
// two seconds, and the client cannot turn the state checked at admission into a
// promise that is no longer true on the completion tick.
func (p *Player) Mount(kind vnet.MountKind) (vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return vnet.RefusalReasonPlayerIsDead, err
	}
	if !p.learnedMounts.Has(kind) {
		return vnet.RefusalReasonMountNotLearned, fmt.Errorf("mount %s is not learned", kind)
	}
	if p.mounted != vnet.MountKindUnknown {
		return vnet.RefusalReasonAlreadyMounted, fmt.Errorf("already mounted on %s", p.mounted)
	}
	if reason, err := p.mountFitLocked(); err != nil {
		return reason, err
	}

	return p.startCastLocked(vnet.CastKindMount, vnet.RefusedActionMount, func() {
		if reason, err := p.mountFitLocked(); err != nil {
			p.queueCastRefusalLocked(protocol.ActionRefused{
				Action: vnet.RefusedActionMount,
				Reason: reason,
			})
			return
		}
		p.mounted = kind
		p.pendingSwing = nil
		p.blocking = false
		p.setMiningLocked(nil)
		p.closeVendorLocked()
	})
}

// Dismount is immediate and unconditional. It also cancels a mount cast without an
// interruption refusal: the player asked to end the mounting state, and absence in
// the next snapshot is the complete answer whether the horse was present or pending.
func (p *Player) Dismount() {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if p.cast != nil && p.cast.kind == vnet.CastKindMount {
		p.cast = nil
	}
	p.mounted = vnet.MountKindUnknown
}

// mountedActionLocked is the common server-side gate for the deliberately
// enumerated saddle restrictions. Inventory movement is absent: mounting does not
// restrict the pack, only actions performed from it.
func (p *Player) mountedActionLocked() (vnet.RefusalReason, error) {
	if p.mounted == vnet.MountKindUnknown {
		return vnet.RefusalReasonUnknown, nil
	}
	return vnet.RefusalReasonActionForbiddenWhileMounted, ErrActionForbiddenWhileMounted
}

// mountFitLocked distinguishes the two spatial refusals, and it is asked twice — at
// admission and again on the completion tick — because a block placed beside the
// caster during the two seconds is a block the completion must not embed the body in.
//
// **The admission sweep is the mounted body itself.** A solid that mountedBody at
// this position overlaps is a low ceiling, whether it is the roof the walking body
// clears by a block or the wall it clears by a tenth; the predicate is [overlaps], the
// same one the movement sweep answers "already inside something" with, so what is
// admitted here is exactly what the next tick can move. A solid above that box but
// below the top of the authoritative streamed cube means the player is indoors.
//
// The caller holds Sim.mu. Terrain reads are non-generating.
func (p *Player) mountFitLocked() (vnet.RefusalReason, error) {
	if !p.onGround {
		return vnet.RefusalReasonMountNotGrounded, errors.New("mounting requires authoritative ground contact")
	}

	fit := mountedBody.boxAt(p.pos)
	if overlaps(p.sim.terrain, fit) {
		return vnet.RefusalReasonMountLowCeiling, fmt.Errorf("the mounted body does not fit at [%.2f %.2f %.2f]", p.pos[0], p.pos[1], p.pos[2])
	}

	x0, x1 := voxelSpan(fit.min[0], fit.max[0])
	z0, z1 := voxelSpan(fit.min[2], fit.max[2])
	roofBottom := int64(math.Ceil(fit.max[1]))
	roofTop := (int64(p.chunk.Y)+int64(p.sim.viewDistance)+1)*world.ChunkSize - 1
	for y := roofBottom; y <= roofTop; y++ {
		for z := z0; z <= z1; z++ {
			for x := x0; x <= x1; x++ {
				block, resident := p.sim.terrain.Block(x, y, z)
				if resident && world.Solid(block) {
					return vnet.RefusalReasonMountIndoors, fmt.Errorf("mounting is indoors under the roof at [%d %d %d]", x, y, z)
				}
			}
		}
	}
	return vnet.RefusalReasonUnknown, nil
}
