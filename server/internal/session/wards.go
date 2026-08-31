package session

import (
	"context"
	"fmt"
	"log/slog"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// snapshotAt couples a complete snapshot to the authoritative column used to build it.
// The session may stream behind the simulation; carrying the column is what prevents a
// newer position from being released under the ward list for an older view.
type snapshotAt struct {
	frame  []byte
	center world.Column
}

// offerLatestSnapshot replaces an older buffered tick without blocking. The simulation
// tick is the sole producer and the ward worker the sole consumer, so after discarding a
// stale value the one remaining frame is always the newest one offered.
func offerLatestSnapshot(snapshots chan snapshotAt, latest snapshotAt) bool {
	select {
	case snapshots <- latest:
		return true
	default:
	}
	select {
	case <-snapshots:
	default:
	}
	select {
	case snapshots <- latest:
		return true
	default:
		return false
	}
}

// followSnapshotsAndWards is the ordered, session-goroutine boundary between the
// simulation's bounded snapshot handoff and the connection writer.
//
// A centre is published only after Streamer.MoveTo has completed, including every
// settlement-materialisation hook. Until the first one arrives this worker retains the
// queued snapshots and sends none. Thereafter a changed centre sends a full WardsNearby
// immediately, while every snapshot checks the runestone revision before it is offered
// to the outbound queue. Both roads converge on sendWards, and the tick goroutine uses
// neither of them.
func followSnapshots(
	ctx context.Context,
	snapshots <-chan snapshotAt,
	offerSnapshot func([]byte) bool,
	log *slog.Logger,
) {
	for {
		select {
		case <-ctx.Done():
			return
		case next := <-snapshots:
			if !offerSnapshot(next.frame) {
				log.Debug("snapshot dropped: the session's outbound queue is full")
			}
		}
	}
}

// followWards keeps the client's copy of the nearby runestone map replaced, from a
// goroutine of its own.
//
// **It used to share a loop with the snapshots, and that is what stopped the character.**
// A snapshot was released only once the streamer had reached the column it described —
// `pending.center == center` — so that a WardsNearby could be placed immediately before
// it. Measured with a player at the controls, that gate held the position for **196, 197,
// 200, 204 and 245 ms**, once per chunk boundary crossed and 1399 ms on the join, and the
// client saw its newest position go 291 ms stale against a 50 ms cadence. The frame rate
// was never touched, which is why this looked for so long like a rendering problem and was
// not one: the position was not late, it was *held*, in a variable, waiting for terrain.
//
// [sendWards] enqueues on the bulk lane and blocks there, so keeping it in the snapshot
// loop would reproduce the stall with the gate removed — a shell of chunk payloads is
// exactly what it would block behind. Two goroutines is what makes "nothing can delay a
// position" true by construction rather than by care.
//
// **What is given up, stated plainly.** WardsNearby is no longer guaranteed to arrive
// immediately before the snapshot that first places the player inside the ward, so for a
// tick or two the boundary may be undrawn where the player already is. That is a
// presentation lag of a translucent wall measured against a character that stopped dead
// for a fifth of a second, and `player/wards.rs` already replaces its set wholesale from
// whatever arrives, so nothing is left inconsistent by the reordering — only late.
func followWards(
	ctx context.Context,
	playerID identity.PlayerID,
	sim *game.Sim,
	radius int32,
	centers <-chan world.Column,
	send func([]byte) error,
	log *slog.Logger,
) {
	var (
		center       world.Column
		haveCenter   bool
		lastCenter   world.Column
		lastRevision uint64
		sentWards    bool
	)

	// A revision changes when somebody raises or breaks a runestone, which no centre
	// change announces. The old loop noticed it because it ran on every snapshot; this
	// one polls at the same rate rather than inheriting that coupling.
	poll := time.NewTicker(wardRevisionPoll)
	defer poll.Stop()

	replace := func() bool {
		if !haveCenter {
			return true
		}
		revision := sim.WardsRevision()
		if sentWards && center == lastCenter && revision == lastRevision {
			return true
		}
		if err := sendWards(playerID, sim, center, radius, send); err != nil {
			if ctx.Err() == nil {
				log.Warn("sending nearby wards failed", "error", err)
			}
			return false
		}
		lastCenter, lastRevision, sentWards = center, revision, true
		return true
	}

	for {
		select {
		case <-ctx.Done():
			return
		case next := <-centers:
			center, haveCenter = next, true
			if !replace() {
				return
			}
		case <-poll.C:
			if !replace() {
				return
			}
		}
	}
}

// wardRevisionPoll is how often the ward list is re-checked without a centre change.
// The simulation runs at 20 Hz and the old loop noticed a revision on every snapshot, so
// this is that rate written down rather than inherited.
const wardRevisionPoll = 50 * time.Millisecond

// sendWards constructs and enqueues one complete replacement, empty list included.
func sendWards(playerID identity.PlayerID, sim *game.Sim, center world.Column, radius int32, send func([]byte) error) error {
	columns := sim.WardsNear(playerID, center, radius)
	frame, err := protocol.EncodeWardsNearby(protocol.WardsNearby{Columns: columns})
	if err != nil {
		return err
	}
	if err := send(frame); err != nil {
		return fmt.Errorf("session: send %d nearby ward columns: %w", len(columns), err)
	}
	return nil
}
