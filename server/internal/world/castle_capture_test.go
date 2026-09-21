package world

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"os"
	"strconv"
	"testing"
)

type castleCaptureHeader struct {
	Magic                               [8]byte
	Format, Worldgen                    uint32
	Seed                                int64
	ActualFacing, ReviewTurn, SceneMode uint32
	BuildingOrigin, Origin              [3]int64
	Size                                [3]uint32
}

func castleCaptureBytes(seed int64, reviewTurn Facing, generated bool) ([]byte, Building) {
	var keep Building
	found := false
	for _, b := range CapitalAt(seed).Buildings {
		if b.Kind == BuildingKeep {
			keep = b
			found = true
			break
		}
	}
	if !found || reviewTurn > 3 || (generated && reviewTurn != 0) {
		panic("invalid castle capture configuration")
	}
	actual := keep
	keep.Facing = Facing((uint8(keep.Facing) + uint8(reviewTurn)) % 4)
	s := SchematicFor(BuildingKeep)
	w, d := rotatedFootprint(s, keep.Facing)
	h := s.H + 1
	origin := [3]int64{keep.OriginX, keep.OriginY - 1, keep.OriginZ}
	if generated {
		// Eight horizontal chunks span the capital neighbourhood; this is a bounded
		// generated region, not a promise to render the whole city or streamed world.
		origin[0] = (floorDiv(keep.OriginX+int64(s.W/2), ChunkSize) - 4) * ChunkSize
		origin[2] = (floorDiv(keep.OriginZ+int64(s.D/2), ChunkSize) - 4) * ChunkSize
		origin[1] = max(int64(0), floorDiv(keep.OriginY-32, ChunkSize)*ChunkSize)
		w, d = 8*ChunkSize, 8*ChunkSize
		h = int((floorDiv(keep.OriginY+int64(s.H)+16+ChunkSize-1, ChunkSize) * ChunkSize) - origin[1])
	}
	if h > 256 || w*h*d > 16_000_000 {
		panic("capture region exceeds loader bound")
	}
	cells := make([]uint16, w*h*d)
	if generated {
		for cy := 0; cy < h/ChunkSize; cy++ {
			for cz := 0; cz < d/ChunkSize; cz++ {
				for cx := 0; cx < w/ChunkSize; cx++ {
					coord := Coord{X: int32(origin[0]/ChunkSize) + int32(cx), Y: int32(origin[1]/ChunkSize) + int32(cy), Z: int32(origin[2]/ChunkSize) + int32(cz)}
					chunk := Generate(seed, coord)
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
	} else {
		for z := range d {
			for x := range w {
				cells[z*w+x] = uint16(Stone)
			}
		}
		visitSchematic(keep, func(x, y, z int64, block Block) {
			cells[(int(y-origin[1])*d+int(z-origin[2]))*w+int(x-origin[0])] = uint16(block)
		})
	}
	header := castleCaptureHeader{Magic: [8]byte{'V', 'H', 'C', 'A', 'S', 'T', '0', '3'}, Format: 3, Worldgen: WorldgenVersion, Seed: seed, ActualFacing: uint32(actual.Facing), ReviewTurn: uint32(reviewTurn), BuildingOrigin: [3]int64{actual.OriginX, actual.OriginY, actual.OriginZ}, Origin: origin, Size: [3]uint32{uint32(w), uint32(h), uint32(d)}}
	if generated {
		header.SceneMode = 1
	}
	var out bytes.Buffer
	for _, v := range []any{header, cells} {
		if err := binary.Write(&out, binary.LittleEndian, v); err != nil {
			panic(err)
		}
	}
	return out.Bytes(), keep
}

func castleReviewPoint(p [3]float64, b Building, turn Facing) [3]float64 {
	s := SchematicFor(BuildingKeep)
	x0, z0 := rotateCell(0, 0, s.W, s.D, turn)
	xx, zx := rotateCell(1, 0, s.W, s.D, turn)
	xz, zz := rotateCell(0, 1, s.W, s.D, turn)
	x, z := p[0]-float64(b.OriginX)-0.5, p[2]-float64(b.OriginZ)-0.5
	return [3]float64{float64(b.OriginX) + float64(x0) + 0.5 + x*float64(xx-x0) + z*float64(xz-x0), p[1], float64(b.OriginZ) + float64(z0) + 0.5 + x*float64(zx-z0) + z*float64(zz-z0)}
}

func TestCastleCaptureExportPreservesPlacedFrames(t *testing.T) {
	if binary.Size(castleCaptureHeader{}) != 96 {
		t.Fatal("capture header size changed")
	}
	for turn := Facing(0); turn < 4; turn++ {
		data, keep := castleCaptureBytes(0x5eed, turn, false)
		var header castleCaptureHeader
		if err := binary.Read(bytes.NewReader(data), binary.LittleEndian, &header); err != nil {
			t.Fatal(err)
		}
		if header.ActualFacing != 0 || header.ReviewTurn != uint32(turn) {
			t.Fatal("actual and review facings conflated")
		}
		w, h, d := int(header.Size[0]), int(header.Size[1]), int(header.Size[2])
		if len(data) != 96+2*w*h*d {
			t.Fatal("fixture length mismatch")
		}
		visitSchematic(keep, func(x, y, z int64, block Block) {
			i := 96 + 2*((int(y-header.Origin[1])*d+int(z-header.Origin[2]))*w+int(x-header.Origin[0]))
			if Block(binary.LittleEndian.Uint16(data[i:i+2])) != block {
				t.Fatal("export differs from placed voxel")
			}
		})
		original := keep
		original.Facing = Facing(header.ActualFacing)
		for z := range 63 {
			for x := range 63 {
				p := castleReviewPoint([3]float64{float64(original.OriginX) + float64(x) + 0.5, float64(original.OriginY), float64(original.OriginZ) + float64(z) + 0.5}, original, turn)
				rx, rz := rotateCell(x, z, 63, 63, turn)
				if p[0] != float64(keep.OriginX)+float64(rx)+0.5 || p[2] != float64(keep.OriginZ)+float64(rz)+0.5 {
					t.Fatal("continuous point and voxel frames differ")
				}
			}
		}
	}
}

func TestExportCastleCaptureFixture(t *testing.T) {
	path := os.Getenv("CASTLE_CAPTURE_FIXTURE")
	if path == "" {
		t.Skip("set CASTLE_CAPTURE_FIXTURE to export")
	}
	seed := int64(0x5eed)
	if value := os.Getenv("CASTLE_CAPTURE_SEED"); value != "" {
		var err error
		seed, err = strconv.ParseInt(value, 0, 64)
		if err != nil {
			t.Fatal(err)
		}
	}
	turn := uint64(0)
	if value := os.Getenv("CASTLE_CAPTURE_REVIEW_TURN"); value != "" {
		var err error
		turn, err = strconv.ParseUint(value, 10, 8)
		if err != nil || turn > 3 {
			t.Fatal("invalid review turn")
		}
	}
	mode := os.Getenv("CASTLE_CAPTURE_MODE")
	if mode != "" && mode != "isolated" && mode != "generated" {
		t.Fatal("unknown capture mode")
	}
	if mode == "generated" && turn != 0 {
		t.Fatal("generated-region review turns require rotating the complete region")
	}
	commit := os.Getenv("CASTLE_CAPTURE_SOURCE_COMMIT")
	decoded, err := hex.DecodeString(commit)
	if err != nil || len(decoded) != 20 {
		t.Fatal("set CASTLE_CAPTURE_SOURCE_COMMIT to the full source commit")
	}
	data, _ := castleCaptureBytes(seed, Facing(turn), mode == "generated")
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(data)
	metadata := struct {
		SourceCommit string `json:"source_commit"`
		SHA256       string `json:"sha256"`
	}{commit, hex.EncodeToString(digest[:])}
	manifest, err := json.MarshalIndent(metadata, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path+".json", append(manifest, '\n'), 0600); err != nil {
		t.Fatal(err)
	}
}

func TestCastleCaptureGeneratedRegionMatchesTerrain(t *testing.T) {
	const seed int64 = 0x5eed
	data, keep := castleCaptureBytes(seed, 0, true)
	again, _ := castleCaptureBytes(seed, 0, true)
	if !bytes.Equal(data, again) {
		t.Fatal("generated fixture is nondeterministic")
	}
	var h castleCaptureHeader
	if err := binary.Read(bytes.NewReader(data), binary.LittleEndian, &h); err != nil {
		t.Fatal(err)
	}
	if h.SceneMode != 1 || h.ReviewTurn != 0 || h.BuildingOrigin != [3]int64{keep.OriginX, keep.OriginY, keep.OriginZ} {
		t.Fatal("generated scene lost actual capital placement")
	}
	// Compare one castle-containing chunk and an exterior corner chunk directly
	// with generation, including AIR: neither is reconstructed from schematic data.
	coords := []Coord{{X: int32(floorDiv(keep.OriginX, ChunkSize)), Y: int32(floorDiv(keep.OriginY, ChunkSize)), Z: int32(floorDiv(keep.OriginZ, ChunkSize))}, {X: int32(h.Origin[0] / ChunkSize), Y: int32(h.Origin[1] / ChunkSize), Z: int32(h.Origin[2] / ChunkSize)}}
	for _, coord := range coords {
		chunk := Generate(seed, coord)
		for y := range ChunkSize {
			for z := range ChunkSize {
				for x := range ChunkSize {
					local := [3]int64{int64(coord.X)*ChunkSize + int64(x) - h.Origin[0], int64(coord.Y)*ChunkSize + int64(y) - h.Origin[1], int64(coord.Z)*ChunkSize + int64(z) - h.Origin[2]}
					i := 96 + 2*((local[1]*int64(h.Size[2])+local[2])*int64(h.Size[0])+local[0])
					if Block(binary.LittleEndian.Uint16(data[i:i+2])) != chunk.At(x, y, z) {
						t.Fatal("generated region differs from actual terrain")
					}
				}
			}
		}
	}
}
