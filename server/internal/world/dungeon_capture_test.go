package world

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"os"
	"strconv"
	"testing"
)

// The first dungeon's capture fixture (#1298): the production instance, voxel for voxel,
// for the client's opt-in zone captures and the boss arena capture.
//
// **Nothing here draws the dungeon a second time.** Every voxel is read out of the chunks
// the production gated instance cache hands a session — the drawing, the seed's rune
// inscription, the gate's doors and mechanisms — so the images show what a party is sent.
// The halo of void chunks around the shell is exported too, as loaded air: the client's
// production rays must not mistake it for terrain that has not arrived.
//
// The **VHDUNG01** header is 96 bytes, little endian:
//
//	0   magic "VHDUNG01"
//	8   format (1), worldgen version; u32
//	16  seed; i64
//	24  the drawing's facing, whether every door and the trapdoor are open; u32
//	32  the placed drawing's origin XYZ (its unrotated cell 0,0,0 at facing 0); i64
//	56  the exported volume's origin XYZ; i64
//	80  the volume's dimensions XYZ; u32
//	92  reserved, zero; u32
//	96  u16 block ids, X fastest, then Z, then Y
type dungeonCaptureHeader struct {
	Magic                  [8]byte
	Format, Worldgen       uint32
	Seed                   int64
	Facing, Opened         uint32
	BuildingOrigin, Origin [3]int64
	Size                   [3]uint32
	Reserved               uint32
}

// dungeonCaptureBytes exports one seed's instance. opened is the state of a party that
// solved everything: the guardian's trapdoor open and every door open through the gate's
// own Update; otherwise the drawing as a party first finds it.
func dungeonCaptureBytes(seed int64, opened bool) ([]byte, error) {
	cache, gate := NewGatedInstanceCache(seed, 1, InstanceChunkEnvelope(seed), opened)
	if opened {
		doors := map[int]bool{}
		for _, a := range InstanceDungeonAnchors(seed) {
			if a.Kind == AnchorInstanceDoor {
				doors[a.Index] = true
			}
		}
		gate.Update(InstanceUpdate{Doors: doors})
	}
	lo, hi := dungeonLayout.chunkBounds(seed)
	lo = Coord{X: lo.X - 1, Y: lo.Y - 1, Z: lo.Z - 1}
	hi = Coord{X: hi.X + 1, Y: hi.Y + 1, Z: hi.Z + 1}
	chunks := [3]int{int(hi.X-lo.X) + 1, int(hi.Y-lo.Y) + 1, int(hi.Z-lo.Z) + 1}
	w, h, d := chunks[0]*ChunkSize, chunks[1]*ChunkSize, chunks[2]*ChunkSize
	cells := make([]uint16, w*h*d)
	for cy := range chunks[1] {
		for cz := range chunks[2] {
			for cx := range chunks[0] {
				coord := Coord{X: lo.X + int32(cx), Y: lo.Y + int32(cy), Z: lo.Z + int32(cz)}
				chunk, _, err := cache.Get(context.Background(), coord)
				if err != nil {
					return nil, err
				}
				for y := range ChunkSize {
					for z := range ChunkSize {
						for x := range ChunkSize {
							cells[((cy*ChunkSize+y)*d+cz*ChunkSize+z)*w+cx*ChunkSize+x] = uint16(chunk.At(x, y, z))
						}
					}
				}
			}
		}
	}
	b := instancePlacement(seed)
	header := dungeonCaptureHeader{
		Magic: [8]byte{'V', 'H', 'D', 'U', 'N', 'G', '0', '1'}, Format: 1, Worldgen: WorldgenVersion,
		Seed: seed, Facing: uint32(b.Facing),
		BuildingOrigin: [3]int64{b.OriginX, b.OriginY, b.OriginZ},
		Origin:         [3]int64{int64(lo.X) * ChunkSize, int64(lo.Y) * ChunkSize, int64(lo.Z) * ChunkSize},
		Size:           [3]uint32{uint32(w), uint32(h), uint32(d)},
	}
	if opened {
		header.Opened = 1
	}
	var out bytes.Buffer
	for _, v := range []any{header, cells} {
		if err := binary.Write(&out, binary.LittleEndian, v); err != nil {
			return nil, err
		}
	}
	return out.Bytes(), nil
}

// The export is the production instance: its frame is the placement's, every voxel of the
// shell is what the gated cache generates, and opening the doors changes exactly the gate's
// cells.
func TestDungeonCaptureExportIsTheGatedInstance(t *testing.T) {
	if binary.Size(dungeonCaptureHeader{}) != 96 {
		t.Fatal("capture header size changed")
	}
	const seed = 0
	shut, err := dungeonCaptureBytes(seed, false)
	if err != nil {
		t.Fatal(err)
	}
	open, err := dungeonCaptureBytes(seed, true)
	if err != nil {
		t.Fatal(err)
	}
	var h dungeonCaptureHeader
	if err := binary.Read(bytes.NewReader(open), binary.LittleEndian, &h); err != nil {
		t.Fatal(err)
	}
	b := instancePlacement(seed)
	if h.Facing != uint32(b.Facing) || h.Opened != 1 || h.BuildingOrigin != [3]int64{b.OriginX, b.OriginY, b.OriginZ} {
		t.Fatalf("header lost the placement: %+v", h)
	}
	w, hh, d := int64(h.Size[0]), int64(h.Size[1]), int64(h.Size[2])
	if int64(len(open)) != 96+2*w*hh*d || len(shut) != len(open) {
		t.Fatal("fixture length mismatch")
	}
	at := func(data []byte, x, y, z int64) Block {
		i := 96 + 2*(((y-h.Origin[1])*d+z-h.Origin[2])*w+x-h.Origin[0])
		return Block(binary.LittleEndian.Uint16(data[i : i+2]))
	}
	// Every exported chunk of the shut drawing is the ungated generator's, except the
	// gate's own cells.
	gated := map[[3]int64]bool{}
	for _, a := range InstanceDungeonAnchors(seed) {
		if a.Kind == AnchorInstanceDoor || a.Kind == AnchorInstanceMechanism {
			gated[[3]int64{a.X, a.Y, a.Z}] = true
		}
	}
	differs := 0
	for y := h.Origin[1]; y < h.Origin[1]+hh; y++ {
		for z := h.Origin[2]; z < h.Origin[2]+d; z++ {
			for x := h.Origin[0]; x < h.Origin[0]+w; x++ {
				if at(shut, x, y, z) != at(open, x, y, z) {
					differs++
				}
			}
		}
	}
	coord := ChunkOf(b.OriginX, b.OriginY, b.OriginZ)
	generated := GenerateInstance(seed, coord)
	ox, oy, oz := coord.Origin()
	for y := range int64(ChunkSize) {
		for z := range int64(ChunkSize) {
			for x := range int64(ChunkSize) {
				if gated[[3]int64{ox + x, oy + y, oz + z}] {
					continue
				}
				if at(shut, ox+x, oy+y, oz+z) != generated.At(int(x), int(y), int(z)) {
					t.Fatalf("exported voxel %d %d %d differs from the generator", ox+x, oy+y, oz+z)
				}
			}
		}
	}
	for _, a := range InstanceDungeonAnchors(seed) {
		if a.Kind == AnchorInstanceDoor && at(open, a.X, a.Y, a.Z) != Air {
			t.Fatalf("door cell %+v is not open in the opened export", a)
		}
	}
	if differs == 0 {
		t.Fatal("opening the doors changed nothing")
	}
	again, err := dungeonCaptureBytes(seed, true)
	if err != nil || !bytes.Equal(again, open) {
		t.Fatal("the export is nondeterministic")
	}
}

func TestExportDungeonCaptureFixture(t *testing.T) {
	path := os.Getenv("DUNGEON_CAPTURE_FIXTURE")
	if path == "" {
		t.Skip("set DUNGEON_CAPTURE_FIXTURE to export")
	}
	seed := int64(0)
	if value := os.Getenv("DUNGEON_CAPTURE_SEED"); value != "" {
		var err error
		if seed, err = strconv.ParseInt(value, 0, 64); err != nil {
			t.Fatal(err)
		}
	}
	state := os.Getenv("DUNGEON_CAPTURE_STATE")
	if state != "" && state != "drawn" && state != "opened" {
		t.Fatal("DUNGEON_CAPTURE_STATE is drawn or opened")
	}
	commit := os.Getenv("DUNGEON_CAPTURE_SOURCE_COMMIT")
	if decoded, err := hex.DecodeString(commit); err != nil || len(decoded) != 20 {
		t.Fatal("set DUNGEON_CAPTURE_SOURCE_COMMIT to the full source commit")
	}
	data, err := dungeonCaptureBytes(seed, state != "drawn")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(data)
	manifest, err := json.MarshalIndent(map[string]string{"source_commit": commit, "sha256": hex.EncodeToString(digest[:])}, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path+".json", append(manifest, '\n'), 0600); err != nil {
		t.Fatal(err)
	}
}
