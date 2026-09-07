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
	r, ok := world.RuinAt(mapTileSeed, x, z)
	if !ok {
		t.Fatal("pinned ruin missing")
	}
	return r
}
func portalRequest(r world.Ruin, scale uint8) protocol.MapTileRequest {
	span := protocol.MapTileSpan(scale)
	floor := func(v int32) int32 { return v - (v%span+span)%span }
	return protocol.MapTileRequest{OriginX: floor(int32(r.Arch.X)), OriginZ: floor(int32(r.Arch.Z)), Scale: scale}
}
func landmarkEntries(t *testing.T, frame []byte) protocol.LandmarkList {
	t.Helper()
	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadLandmarkList {
		t.Fatal("not a landmark frame")
	}
	var tab flatbuffers.Table
	if !env.Payload(&tab) {
		t.Fatal("missing list")
	}
	var list vnet.LandmarkList
	list.Init(tab.Bytes, tab.Pos)
	out := protocol.LandmarkList{OriginX: list.OriginX(), OriginZ: list.OriginZ(), Scale: list.Scale(), Landmarks: make([]protocol.Landmark, list.LandmarksLength())}
	for i := range out.Landmarks {
		var l vnet.Landmark
		if !list.Landmarks(&l, i) {
			t.Fatal("missing entry")
		}
		out.Landmarks[i] = protocol.Landmark{LandmarkID: l.LandmarkId(), X: l.X(), Z: l.Z(), Kind: l.Kind(), Discovered: l.Discovered()}
	}
	return out
}

func TestPortalKnowledgePrecedesDiscoveryAndSurvivesWorldReload(t *testing.T) {
	dir := t.TempDir()
	storedWorld, err := world.OpenStore(dir, mapTileSeed)
	if err != nil {
		t.Fatal(err)
	}
	cache := world.NewPersistentCache(storedWorld, 1, 8)
	store, character := exploringCharacter(t, dir)
	e := newExploration(store, character.ID, nil, false, nil)
	r := landmarkFixture(t, 0, 0)
	request := portalRequest(r, 1)
	before := landmarksForTile(cache.Seed(), request, e)
	if len(before.Landmarks) != 1 || before.Landmarks[0].Discovered {
		t.Fatal("unexplored portal must be supplied as undiscovered")
	}
	arch := world.ChunkOf(r.Arch.X, 0, r.Arch.Z).Column()
	neighbour := world.Column{CX: arch.CX + 1, CZ: arch.CZ}
	e.Reveal(neighbour)
	if landmarksForTile(cache.Seed(), request, e).Landmarks[0].Discovered {
		t.Fatal("neighbour discovered portal")
	}
	e.Reveal(arch)
	want := landmarksForTile(cache.Seed(), request, e)
	if !want.Landmarks[0].Discovered || want.Landmarks[0].LandmarkID != before.Landmarks[0].LandmarkID {
		t.Fatal("discovery moved or renamed portal")
	}
	if err := e.Save(); err != nil {
		t.Fatal(err)
	}
	storedWorld, err = world.OpenStore(dir, mapTileSeed)
	if err != nil {
		t.Fatal(err)
	}
	cache = world.NewPersistentCache(storedWorld, 1, 8)
	reopened, err := persist.OpenExplorationStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	cols, found, err := reopened.Load(character.ID)
	if err != nil || !found {
		t.Fatalf("reload: %v %v", found, err)
	}
	got := landmarksForTile(cache.Seed(), request, newExploration(reopened, character.ID, cols, false, nil))
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("reloaded %+v want %+v", got, want)
	}
	if landmarksForTile(cache.Seed(), request, mapTileLedger()).Landmarks[0].Discovered {
		t.Fatal("discovery crossed character boundary")
	}
}

func TestPortalTileLookupIsBoundedAndStableAcrossScales(t *testing.T) {
	r := landmarkFixture(t, 0, 0)
	var id uint64
	for _, scale := range protocol.MapTileScales {
		span := protocol.MapTileSpan(scale)
		if world.RuinCellBlocks%int(span) != 0 {
			t.Fatal("tile may span multiple ruin cells")
		}
		req := portalRequest(r, scale)
		list := landmarksForTile(mapTileSeed, req, nil)
		if len(list.Landmarks) != 1 {
			t.Fatalf("scale %d lost portal", scale)
		}
		l := list.Landmarks[0]
		if id != 0 && id != l.LandmarkID {
			t.Fatal("zoom renamed portal")
		}
		id = l.LandmarkID
		if l.X != int32(r.Arch.X) || l.Z != int32(r.Arch.Z) {
			t.Fatal("site is not arch")
		}
		req.OriginX += span
		if len(landmarksForTile(mapTileSeed, req, nil).Landmarks) != 0 {
			t.Fatal("neighbour tile includes another scope's portal")
		}
		for _, origin := range []int32{math.MinInt32, -span, 0, math.MaxInt32 - (span - 1)} {
			first := world.RuinCellOf(int64(origin))
			last := world.RuinCellOf(int64(origin) + int64(span) - 1)
			if first != last {
				t.Fatal("valid tile crosses ruin cell")
			}
			extreme := landmarksForTile(mapTileSeed, protocol.MapTileRequest{OriginX: origin, OriginZ: math.MinInt32, Scale: scale}, nil)
			if len(extreme.Landmarks) != 0 {
				t.Fatal("out-of-world request returned a site")
			}
		}
	}
}

func TestMapRequestSendsUndiscoveredPortalWhileTerrainStaysFogged(t *testing.T) {
	e := mapTileLedger()
	var frames [][]byte
	send := func(f []byte) error { frames = append(frames, f); return nil }
	log := slog.New(slog.DiscardHandler)
	clock := &stoppedClock{}
	s := NewStreamer(world.NewCache(mapTileSeed, 1, 8), 0, send, func() {}, clock.now, log)
	s.RecordExploration(e)
	r := landmarkFixture(t, 0, 0)
	req := portalRequest(r, 1)
	msg, err := protocol.Decode(protocol.EncodeMapTileRequest(req))
	if err != nil {
		t.Fatal(err)
	}
	for i := 0; i < mapTileBurst; i++ {
		if err := handlePostHandshake(context.Background(), msg, nil, s, nil, nil, send, log); err != nil {
			t.Fatal(err)
		}
	}
	if len(frames) != 2*mapTileBurst {
		t.Fatalf("request did not return exactly terrain+landmark pair: %d", len(frames))
	}
	for i := 0; i < len(frames); i += 2 {
		env := vnet.GetRootAsEnvelope(frames[i], 0)
		if env.PayloadType() != vnet.PayloadMapTile {
			t.Fatal("terrain response missing")
		}
		var tab flatbuffers.Table
		if !env.Payload(&tab) {
			t.Fatal("terrain missing")
		}
		var tile vnet.MapTile
		tile.Init(tab.Bytes, tab.Pos)
		for j := 0; j < tile.HeightLength(); j++ {
			if tile.Height(j) != 0 || tile.Surface(j) != 0 {
				t.Fatal("terrain fog leaked")
			}
		}
		for j := 0; j < tile.ExploredLength(); j++ {
			if tile.Explored(j) != 0 {
				t.Fatal("request explored terrain")
			}
		}
		list := landmarkEntries(t, frames[i+1])
		if len(list.Landmarks) != 1 || list.Landmarks[0].Discovered || list.OriginX != req.OriginX || list.OriginZ != req.OriginZ || list.Scale != req.Scale {
			t.Fatal("incorrect portal knowledge")
		}
	}
	frames = nil
	if err := handlePostHandshake(context.Background(), msg, nil, s, nil, nil, send, log); err != nil {
		t.Fatal(err)
	}
	if len(frames) != 0 {
		t.Fatal("landmark bypassed tile limiter")
	}
	if e.Count() != 0 || len(s.landmarks.cells) != 0 || len(s.landmarks.found) != 0 {
		t.Fatal("requests mutated exploration or discovery cache")
	}
}

func TestStreamingPushesCanonicalDiscoveryWithoutMapRequest(t *testing.T) {
	e := mapTileLedger()
	var frames [][]byte
	s := NewStreamer(world.NewCache(mapTileSeed, 1, 8), 0, func(f []byte) error { frames = append(frames, f); return nil }, func() {}, time.Now, slog.New(slog.DiscardHandler))
	s.RecordExploration(e)
	if len(frames) != 0 {
		t.Fatal("initial global list is forbidden")
	}
	// A world holds one portal (#1020), so where this walked two sites of one world it
	// now walks the one it has. The second half — moving back into the same view — is
	// unchanged and is what pins that discovery is not re-sent.
	r := landmarkFixture(t, 0, 0)
	frames = nil
	coord := world.ChunkOf(r.Arch.X, r.Arch.Y, r.Arch.Z)
	if err := s.MoveTo(context.Background(), coord); err != nil {
		t.Fatal(err)
	}
	if len(frames) < 3 || vnet.GetRootAsEnvelope(frames[len(frames)-2], 0).PayloadType() != vnet.PayloadMapExplored {
		t.Fatal("discovery must follow exploration")
	}
	list := landmarkEntries(t, frames[len(frames)-1])
	want := portalRequest(r, 1)
	if list.OriginX != want.OriginX || list.OriginZ != want.OriginZ || list.Scale != 1 || len(list.Landmarks) != 1 || !list.Landmarks[0].Discovered {
		t.Fatal("discovery is not one canonical tile")
	}
	frames = nil
	if err := s.MoveTo(context.Background(), coord); err != nil {
		t.Fatal(err)
	}
	if len(frames) != 0 {
		t.Fatal("unchanged view resent discovery")
	}
}

func TestFailedChunkSendNeverDiscoversALandmark(t *testing.T) {
	e := mapTileLedger()
	blocked := errors.New("test send failed")
	s := NewStreamer(world.NewCache(mapTileSeed, 1, 8), 0, func([]byte) error { return blocked }, func() {}, time.Now, slog.New(slog.DiscardHandler))
	s.RecordExploration(e)
	r := landmarkFixture(t, 0, 0)
	if err := s.MoveTo(context.Background(), world.ChunkOf(r.Arch.X, r.Arch.Y, r.Arch.Z)); !errors.Is(err, blocked) {
		t.Fatal(err)
	}
	if e.Count() != 0 || len(s.landmarks.found) != 0 {
		t.Fatal("unsent chunk discovered portal")
	}
}

func BenchmarkPortalTileLookup(b *testing.B) {
	r, ok := world.RuinAt(mapTileSeed, 0, 0)
	if !ok {
		b.Fatal("fixture missing")
	}
	req := portalRequest(r, 1)
	for b.Loop() {
		if len(landmarksForTile(mapTileSeed, req, nil).Landmarks) != 1 {
			b.Fatal("portal lost")
		}
	}
}
