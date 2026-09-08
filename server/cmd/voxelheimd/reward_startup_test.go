package main

import (
	"encoding/binary"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Independent v1 fixture: an empty high-water journal or one retained run with no
// defeat yet. Even that run is not safe for an inactive reader to silently ignore.
func startupRewardFixture(active bool) []byte {
	b := world.NewRecord(world.HeaderSize, 0, [4]byte{'V', 'X', 'H', 'R'}, persist.BossRewardsVersion)[:world.HeaderSize]
	b = binary.LittleEndian.AppendUint64(b, 1)
	b = binary.LittleEndian.AppendUint64(b, 42)
	count := uint32(0)
	if active {
		count = 1
	}
	b = binary.LittleEndian.AppendUint32(b, count)
	if active {
		b = binary.LittleEndian.AppendUint64(b, 41) // durable generation
		b = binary.LittleEndian.AppendUint32(b, world.WorldgenVersion)
		for _, n := range []uint64{7, 19, 2, 3, 100} {
			b = binary.LittleEndian.AppendUint64(b, n)
		}
		b = binary.LittleEndian.AppendUint32(b, 0) // bindings
		b = binary.LittleEndian.AppendUint32(b, 0) // defeated bosses
	}
	b = binary.LittleEndian.AppendUint32(b, 0) // prepared intents
	b = append(b, make([]byte, world.ChecksumSize)...)
	world.PutChecksum(b)
	return b
}

func TestStartupRefusesPendingOrCorruptRewardsBeforeOpeningPlayers(t *testing.T) {
	for _, corrupt := range []bool{false, true} {
		dir := t.TempDir()
		b := startupRewardFixture(true)
		if corrupt {
			b[len(b)-1] ^= 1
		}
		if err := os.WriteFile(filepath.Join(dir, "boss-rewards.bin"), b, 0600); err != nil {
			t.Fatal(err)
		}
		_, err := openPlayers(options{worldDir: dir}, discard())
		want := persist.ErrRewardReplayRequired
		if corrupt {
			want = world.ErrCorruptStore
		}
		if !errors.Is(err, want) {
			t.Fatalf("startup did not refuse reward state: %v", err)
		}
		if _, err := os.Stat(filepath.Join(dir, "players")); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("refused startup modified players")
		}
	}
}

func TestStartupAcceptsHighWaterOnlyRewardsAndEphemeralWorld(t *testing.T) {
	dir := t.TempDir()
	b := startupRewardFixture(false)
	if err := os.WriteFile(filepath.Join(dir, "boss-rewards.bin"), b, 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := openPlayers(options{worldDir: dir}, discard()); err != nil {
		t.Fatal(err)
	}
	if s, err := openPlayers(options{}, discard()); err != nil || s != nil {
		t.Fatal("ephemeral startup changed")
	}
}
