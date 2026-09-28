package persist

import (
	"encoding/binary"
	"errors"
	"os"
	"reflect"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Encode the old v2 layout independently: a new encoder cannot prove that the
// released format remains readable merely by changing its own version number.
func versionTwoSessionsFile(records []SessionRecord) []byte {
	const head = 46
	body := 0
	for _, rec := range records {
		body += head + len(rec.DefeatedBosses) + len(rec.Bound)*40 + len(rec.SolvedPuzzles) + len(rec.ClearedGroups)
	}
	buf := world.NewRecord(sessionsHeaderSize, body, sessionsMagic, 2)
	binary.LittleEndian.PutUint32(buf[offSessionCount:offSessionCount+4], uint32(len(records)))
	at := sessionsHeaderSize
	for _, rec := range records {
		binary.LittleEndian.PutUint64(buf[at:at+8], rec.ID)
		binary.LittleEndian.PutUint64(buf[at+8:at+16], uint64(rec.Seed))
		binary.LittleEndian.PutUint64(buf[at+16:at+24], uint64(rec.Ruin[0]))
		binary.LittleEndian.PutUint64(buf[at+24:at+32], uint64(rec.Ruin[1]))
		binary.LittleEndian.PutUint64(buf[at+32:at+40], uint64(rec.ExpiresUnix))
		buf[at+40] = byte(len(rec.DefeatedBosses))
		binary.LittleEndian.PutUint16(buf[at+41:at+43], uint16(len(rec.Bound)))
		buf[at+43], buf[at+44], buf[at+45] = rec.Checkpoints, byte(len(rec.SolvedPuzzles)), byte(len(rec.ClearedGroups))
		at += head
		for _, kind := range rec.DefeatedBosses {
			buf[at] = byte(kind)
			at++
		}
		for _, who := range rec.Bound {
			copy(buf[at:at+identity.IDSize], who.PlayerID[:])
			binary.LittleEndian.PutUint64(buf[at+32:at+40], who.CharacterID)
			at += 40
		}
		at += copy(buf[at:], rec.SolvedPuzzles)
		at += copy(buf[at:], rec.ClearedGroups)
	}
	world.PutChecksum(buf)
	return buf
}

func TestSessionStoreMigratesVersionTwoPreservingRoute(t *testing.T) {
	want := runsFixture()
	for i := range want {
		want[i].OpenedChests = nil
	}
	store := openRuns(t, t.TempDir())
	if err := os.WriteFile(store.Path(), versionTwoSessionsFile(want), 0o644); err != nil {
		t.Fatal(err)
	}
	got, found, err := store.Load()
	if err != nil || !found || !reflect.DeepEqual(got, want) {
		t.Fatalf("v2 migration: %#v, found %v, err %v", got, found, err)
	}
	if err := store.Save(got); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(store.Path())
	if err != nil {
		t.Fatal(err)
	}
	if version := binary.LittleEndian.Uint32(data[4:8]); version != 3 {
		t.Fatalf("migrated version = %d", version)
	}
	again, _, err := store.Load()
	if err != nil || !reflect.DeepEqual(again, want) {
		t.Fatalf("migrated v3 lost route: %#v, %v", again, err)
	}
}

func TestSessionsRejectOversizedOrTruncatedChestLists(t *testing.T) {
	if _, err := encodeSessions([]SessionRecord{{OpenedChests: make([]uint8, MaxOpenedChests+1)}}); !errors.Is(err, ErrTooManySessions) {
		t.Fatalf("oversized writer list: %v", err)
	}
	for _, count := range []byte{1, MaxOpenedChests + 1} {
		data, err := encodeSessions([]SessionRecord{{ID: 1}})
		if err != nil {
			t.Fatal(err)
		}
		data[sessionsHeaderSize+46] = count
		world.PutChecksum(data)
		if _, err := decodeSessions(data); !errors.Is(err, world.ErrCorruptStore) {
			t.Fatalf("invalid chest count %d: %v", count, err)
		}
	}
}
