package main

import (
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func (pt *party) restartHoard(ctx context.Context, o options, old *serverProcess, pair *ticket.Pair, worldID ticket.WorldID) (result error) {
	// Original readers/drivers have ended before this method. Close each transport,
	// flush normally, then start a new process using the same directory and identities.
	saved := make([]protocol.InventoryState, len(pt.members))
	for i, r := range pt.members {
		r.hoard.Stage = "server shutdown and restart"
		saved[i], _ = r.c.inventoryAnswer()
		_ = r.c.conn.Close()
	}
	if err := old.shutdown(); err != nil {
		return fmt.Errorf("pre-restart persistence: %w", err)
	}
	server, err := startServer(ctx, o, pair.PublicHex())
	if err != nil {
		return err
	}
	defer func() { result = errors.Join(result, server.shutdown()) }()
	session, cancel := context.WithCancel(ctx)
	readers := make(chan error, len(pt.members))
	joined := 0
	defer func() {
		cancel()
		for i := 0; i < joined; i++ {
			_ = pt.members[i].c.conn.Close()
		}
		for i := 0; i < joined; i++ {
			result = errors.Join(result, <-readers)
		}
	}()
	allies := map[uint64]bool{}
	for i, r := range pt.members {
		r.hoard.Stage = "existing character rejoin"
		sum := sha256.Sum256([]byte(r.c.name))
		var account ticket.AccountID
		copy(account[:], sum[:])
		credential, _, err := pair.Mint(account, worldID, time.Now())
		if err != nil {
			return err
		}
		c, err := join(ctx, server.addr, server.fingerprint, r.c.name, credential[:], r.stats, true)
		if err != nil {
			return fmt.Errorf("rejoin member %d: %w", i+1, err)
		}
		r.c, r.server = c, server
		joined++
		allies[c.entityID] = true
		go func() { readers <- c.listen(session) }()
		go c.drive(session, tickRate)
		if _, err := r.inventoryAfter(ctx, 0, func(s protocol.InventoryState) bool { return inventoriesEqual(s, saved[i]) }); err != nil {
			return fmt.Errorf("member %d inventory after restart: %w", i+1, err)
		}
	}
	for _, r := range pt.members {
		r.allies = allies
	}
	if err := pt.form(ctx); err != nil {
		return fmt.Errorf("reform saved party: %w", err)
	}
	// Entry offers are a party vote: all members must drive their portal/offer loops.
	entries := make(chan error, len(pt.members))
	for _, r := range pt.members {
		r.hoard.Stage = "saved run portal"
		go func() { entries <- r.enterPortal(session) }()
	}
	var entryErr error
	for range pt.members {
		if err := <-entries; err != nil {
			entryErr = errors.Join(entryErr, err)
			cancel()
		}
	}
	if entryErr != nil {
		return fmt.Errorf("saved-run portal: %w", entryErr)
	}
	for _, r := range pt.members {
		if r.c.self().world != r.hoard.WorldID || r.lay.seed != r.hoard.Seed {
			return errors.New("portal rejoin selected a different saved run")
		}
	}
	// All three members are back in the same saved run. Each walks to each chest:
	// no teleport into the dungeon and no terrain/state injection is allowed.
	for _, r := range pt.members {
		for index := 0; index < world.InstanceChestCount; index++ {
			r.hoard.Stage = fmt.Sprintf("saved chest %d", index+1)
			if index == world.SandHallChest {
				if err := r.drop(ctx); err != nil {
					return err
				}
				// Unlike the solved rune/twin doors, this timed grille closes again.
				if err := r.timedGrille(ctx); err != nil {
					return err
				}
			}
			if index == world.KingChest {
				if err := r.waitOpen(ctx, r.lay.twinDoor, 10*time.Second); err != nil {
					return fmt.Errorf("saved twin-lever door: %w", err)
				}
			}
			anchor, err := chestAnchor(r.lay.seed, index)
			if err != nil {
				return err
			}
			if err := r.walkTo(ctx, "saved open chest", near(at(anchor), 1.5), true); err != nil {
				return err
			}
			if err := r.waitHoard(ctx, "persisted ChestOpen", func() bool { b, ok := r.c.blockAt(at(anchor)); return ok && b == world.ChestOpen }); err != nil {
				return err
			}
			if err := r.chestIntent(anchor); err != nil {
				return err
			}
			if _, err := r.awaitLoot(ctx, 0, false, vnet.RefusalReasonChestAlreadyOpened); err != nil {
				return err
			}
			r.hoard.Restored[index] = true
			r.say("hoard restart: same saved run, chest %d remains open and exhausted", index+1)
		}
	}
	return nil
}

func inventoriesEqual(a, b protocol.InventoryState) bool {
	if a.Silver != b.Silver || len(a.Stacks) != len(b.Stacks) {
		return false
	}
	for i := range a.Stacks {
		if a.Stacks[i] != b.Stacks[i] {
			return false
		}
	}
	return true
}
