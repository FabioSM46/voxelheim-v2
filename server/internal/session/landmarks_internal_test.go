package session

import (
	"context"
	"errors"
	"log/slog"
	"math"
	"reflect"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	flatbuffers "github.com/google/flatbuffers/go"
)

func landmarkFixture(t *testing.T, x, z int64) world.Ruin {
	t.Helper()
	ruin, ok := world.RuinAt(mapTileSeed, x, z)
	if !ok {
		t.Fatal("pinned ruin no longer exists")
	}
	return ruin
}

func TestLandmarksRequireTheActualArchColumnAndSurviveReload(t *testing.T) {
	dir := t.TempDir()
	storedWorld, err := world.OpenStore(dir, mapTileSeed)
	if err != nil {
		t.Fatal(err)
	}
	chunks := world.NewPersistentCache(storedWorld, 1, 8)
	store, character := exploringCharacter(t, dir)
	explored := newExploration(store, character.ID, nil, false, nil)
	ruin := landmarkFixture(t, -6, 6)
	arch := world.ChunkOf(ruin.Arch.X, 0, ruin.Arch.Z).Column()
	neighbour := world.Column{CX: arch.CX + 1, CZ: arch.CZ}
	explored.Reveal(neighbour)
	l := newLandmarks(chunks.Seed(), explored)
	if len(l.list().Landmarks) != 0 {
		t.Fatal("neighbour revealed a portal")
	}
	if len(l.cells) != 1 {
		t.Fatal("columns not deduplicated by ruin cell")
	}
	explored.Reveal(arch)
	if !l.reveal(explored.TakeRevealed()) {
		t.Fatal("actual column failed to reveal portal")
	}
	want := l.list()
	if len(want.Landmarks) != 1 {
		t.Fatal("no portal after exploration")
	}
	got := want.Landmarks[0]
	if got.LandmarkID == 0 || got.X != int32(ruin.Arch.X) || got.Z != int32(ruin.Arch.Z) || got.Kind != vnet.LandmarkKindPortal {
		t.Fatalf("wrong landmark: %+v", got)
	}
	// Idempotent exploration and a fresh character never inherit this discovery.
	if l.reveal([]world.Column{arch, arch}) || len(newLandmarks(mapTileSeed, mapTileLedger()).list().Landmarks) != 0 {
		t.Fatal("duplicate or cross-character discovery")
	}
	if err := explored.Save(); err != nil {
		t.Fatal(err)
	}
	// Drop live derivation and reopen the existing ledger directory. No landmark
	// file is involved, so a process restart has exactly the same inputs.
	storedWorld, err = world.OpenStore(dir, mapTileSeed)
	if err != nil {
		t.Fatal(err)
	}
	chunks = world.NewPersistentCache(storedWorld, 1, 8)
	reopened, err := persist.OpenExplorationStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	stored, found, err := reopened.Load(character.ID)
	if err != nil || !found {
		t.Fatalf("reload: %v,%v", found, err)
	}
	again := newLandmarks(chunks.Seed(), newExploration(reopened, character.ID, stored, false, nil)).list()
	if !reflect.DeepEqual(again, want) {
		t.Fatalf("restart moved discovery: %+v want %+v", again, want)
	}
}

func TestLandmarkListsAreCompleteStableAndBoundedByExploration(t *testing.T) {
	first, second := landmarkFixture(t, -6, 6), landmarkFixture(t, 0, -12)
	cols := []world.Column{world.ChunkOf(first.Arch.X, 0, first.Arch.Z).Column(), world.ChunkOf(second.Arch.X, 0, second.Arch.Z).Column()}
	a := newLandmarks(mapTileSeed, mapTileLedger(cols...)).list()
	b := newLandmarks(mapTileSeed, mapTileLedger(cols[1], cols[0])).list()
	if len(a.Landmarks) != 2 || !reflect.DeepEqual(a, b) || a.Landmarks[0].LandmarkID == a.Landmarks[1].LandmarkID {
		t.Fatal("order-dependent or colliding identities")
	}
	a.Landmarks[0].X = 0
	if reflect.DeepEqual(a, b) {
		t.Fatal("lists share mutable storage")
	}
	extreme := mapTileLedger(world.Column{CX: math.MaxInt32, CZ: math.MinInt32}, world.Column{CX: math.MinInt32, CZ: math.MaxInt32})
	if len(newLandmarks(mapTileSeed, extreme).list().Landmarks) != 0 {
		t.Fatal("out-of-world ledger produced a portal")
	}
}

func landmarkStreamer(t *testing.T, explored *Exploration, send func([]byte) error) *Streamer {
	t.Helper()
	s := NewStreamer(world.NewCache(mapTileSeed, 1, 8), 0, send, func() {}, time.Now, slog.New(slog.DiscardHandler))
	s.RecordExploration(explored)
	if err := s.SendLandmarks(); err != nil {
		t.Fatal(err)
	}
	return s
}

func landmarkEntries(t *testing.T, frame []byte) []protocol.Landmark {
	t.Helper()
	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadLandmarkList {
		t.Fatal("not a landmark frame")
	}
	var tab flatbuffers.Table
	if !env.Payload(&tab) {
		t.Fatal("absent list")
	}
	var list vnet.LandmarkList
	list.Init(tab.Bytes, tab.Pos)
	out := make([]protocol.Landmark, list.LandmarksLength())
	for i := range out {
		var l vnet.Landmark
		if !list.Landmarks(&l, i) {
			t.Fatal("absent landmark")
		}
		out[i] = protocol.Landmark{LandmarkID: l.LandmarkId(), X: l.X(), Z: l.Z(), Kind: l.Kind()}
	}
	return out
}

func TestStreamingSendsWholeLandmarkListAfterExploration(t *testing.T) {
	explored := mapTileLedger()
	var frames [][]byte
	s := landmarkStreamer(t, explored, func(f []byte) error { frames = append(frames, f); return nil })
	if len(frames) != 1 || len(landmarkEntries(t, frames[0])) != 0 {
		t.Fatal("join must send an empty list")
	}
	for i, ruin := range []world.Ruin{landmarkFixture(t, -6, 6), landmarkFixture(t, 0, -12)} {
		frames = nil
		coord := world.ChunkOf(ruin.Arch.X, ruin.Arch.Y, ruin.Arch.Z)
		if err := s.MoveTo(context.Background(), coord); err != nil {
			t.Fatal(err)
		}
		if len(frames) < 3 {
			t.Fatal("missing streamed discovery")
		}
		if vnet.GetRootAsEnvelope(frames[len(frames)-2], 0).PayloadType() != vnet.PayloadMapExplored {
			t.Fatal("landmarks must follow MapExplored")
		}
		if got := landmarkEntries(t, frames[len(frames)-1]); len(got) != i+1 {
			t.Fatalf("replacement holds %d, want %d", len(got), i+1)
		}
		frames = nil
		if err := s.MoveTo(context.Background(), coord); err != nil {
			t.Fatal(err)
		}
		if len(frames) != 0 {
			t.Fatal("unchanged view resent discovery")
		}
	}
}

func TestFailedChunkSendNeverRevealsALandmark(t *testing.T) {
	explored := mapTileLedger()
	blocked := errors.New("test send failed")
	s := landmarkStreamer(t, explored, func([]byte) error { return nil })
	s.send = func([]byte) error { return blocked }
	r := landmarkFixture(t, -6, 6)
	if err := s.MoveTo(context.Background(), world.ChunkOf(r.Arch.X, r.Arch.Y, r.Arch.Z)); !errors.Is(err, blocked) {
		t.Fatalf("send: %v", err)
	}
	if explored.Count() != 0 || len(s.landmarks.list().Landmarks) != 0 {
		t.Fatal("unsent chunk revealed arch")
	}
}

func TestClientRequestSurfaceCannotForgeLandmarkDiscovery(t *testing.T) {
	explored := mapTileLedger()
	var frames [][]byte
	send := func(f []byte) error { frames = append(frames, f); return nil }
	s := landmarkStreamer(t, explored, send)
	frames = nil
	log := slog.New(slog.DiscardHandler)
	// Sweep every named tag through the actual decoder and post-handshake router.
	// Absent mandatory fields are refused at decode; valid empty requests and
	// server-only/unknown messages still cannot write the ledger or emit landmarks.
	for tag := range vnet.EnumNamesPayload {
		b := flatbuffers.NewBuilder(64)
		b.StartObject(0)
		empty := b.EndObject()
		vnet.EnvelopeStart(b)
		vnet.EnvelopeAddPayloadType(b, tag)
		vnet.EnvelopeAddPayload(b, empty)
		b.FinishWithFileIdentifier(vnet.EnvelopeEnd(b), []byte("VXLH"))
		msg, err := protocol.Decode(b.FinishedBytes())
		if err == nil {
			_ = handlePostHandshake(context.Background(), msg, nil, s, nil, NewRegistry(DefaultConcurrentSessions), send, log)
		}
	}
	// A request naming the actual remote arch is still a request, not exploration.
	r := landmarkFixture(t, -6, 6)
	_ = s.Resend(world.ChunkOf(r.Arch.X, r.Arch.Y, r.Arch.Z))
	_, _ = s.DrawMapTile(protocol.MapTileRequest{OriginX: int32(r.Arch.X / 64 * 64), OriginZ: int32(r.Arch.Z / 64 * 64), Scale: 1})
	for _, f := range frames {
		if vnet.GetRootAsEnvelope(f, 0).PayloadType() == vnet.PayloadLandmarkList {
			t.Fatal("request emitted landmarks")
		}
	}
	if explored.Count() != 0 || len(s.landmarks.list().Landmarks) != 0 {
		t.Fatal("request changed discovery")
	}
}
