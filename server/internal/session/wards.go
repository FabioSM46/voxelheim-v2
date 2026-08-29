package session

import (
	"context"
	"fmt"
	"log/slog"

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
func followSnapshotsAndWards(
	ctx context.Context,
	playerID identity.PlayerID,
	sim *game.Sim,
	radius int32,
	centers <-chan world.Column,
	snapshots <-chan snapshotAt,
	send func([]byte) error,
	offerSnapshot func([]byte) bool,
	log *slog.Logger,
) {
	var (
		center       world.Column
		haveCenter   bool
		lastCenter   world.Column
		lastRevision uint64
		sentWards    bool
		pending      *snapshotAt
	)

	for {
		// A snapshot is released only after the initial centre exists and any changed
		// runestone map has replaced the client's copy. It remains non-blocking at the
		// outbound queue; the next tick supersedes it if the writer is behind.
		if haveCenter && pending != nil && pending.center == center {
			// Prefer the newest buffered tick before doing any work. The same drain is
			// repeated after sendWards because that ordered send may wait behind a full
			// writer queue while the tick continues replacing the one-entry handoff.
			select {
			case next := <-snapshots:
				pending = &next
				continue
			default:
			}
			revision := sim.WardsRevision()
			if !sentWards || center != lastCenter || revision != lastRevision {
				if err := sendWards(playerID, sim, center, radius, send); err != nil {
					if ctx.Err() == nil {
						log.Warn("sending nearby wards failed", "error", err)
					}
					return
				}
				lastCenter, lastRevision, sentWards = center, revision, true
			}
			select {
			case next := <-snapshots:
				pending = &next
				continue
			default:
			}
			if !offerSnapshot(pending.frame) {
				log.Debug("snapshot dropped: the session's outbound queue is full")
			}
			pending = nil
		}

		select {
		case <-ctx.Done():
			return
		case next := <-centers:
			center, haveCenter = next, true
			if !sentWards || center != lastCenter {
				revision := sim.WardsRevision()
				if err := sendWards(playerID, sim, center, radius, send); err != nil {
					if ctx.Err() == nil {
						log.Warn("sending nearby wards failed", "error", err)
					}
					return
				}
				lastCenter, lastRevision, sentWards = center, revision, true
			}
		case next := <-snapshots:
			pending = &next
		}
	}
}

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
