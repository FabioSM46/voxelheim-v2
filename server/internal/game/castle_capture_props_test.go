package game

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"os"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The capture consumes the real wire encoder and static-prop index, never a second catalogue.
func TestExportCastleCaptureSnapshot(t *testing.T) {
	path := os.Getenv("CASTLE_CAPTURE_FIXTURE")
	if path == "" {
		t.Skip("opt-in server snapshot export")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(data) < 96 || string(data[:8]) != "VHCAST03" {
		t.Fatal("invalid fixture")
	}
	seed := int64(binary.LittleEndian.Uint64(data[16:24]))
	turn := binary.LittleEndian.Uint32(data[28:32])
	if turn > 3 {
		t.Fatal("invalid review turn")
	}
	origin := [3]int64{}
	for i := range 3 {
		origin[i] = int64(binary.LittleEndian.Uint64(data[36+i*8 : 44+i*8]))
	}
	commit := os.Getenv("CASTLE_CAPTURE_SOURCE_COMMIT")
	decoded, err := hex.DecodeString(commit)
	if err != nil || len(decoded) != 20 {
		t.Fatal("full source commit required")
	}
	var fixtureMetadata struct {
		SourceCommit string `json:"source_commit"`
		SHA256       string `json:"sha256"`
	}
	sidecar, err := os.ReadFile(path + ".json")
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(sidecar, &fixtureMetadata); err != nil {
		t.Fatal(err)
	}
	fixtureHash := sha256.Sum256(data)
	if fixtureMetadata.SourceCommit != commit || fixtureMetadata.SHA256 != hex.EncodeToString(fixtureHash[:]) {
		t.Fatal("fixture provenance mismatch")
	}
	var keep *world.Building
	capital := world.CapitalAt(seed)
	for i := range capital.Buildings {
		if capital.Buildings[i].Kind == world.BuildingKeep {
			keep = &capital.Buildings[i]
			break
		}
	}
	if keep == nil || origin != [3]int64{keep.OriginX, keep.OriginY, keep.OriginZ} || binary.LittleEndian.Uint32(data[24:28]) != uint32(keep.Facing) {
		t.Fatal("fixture does not describe this capital")
	}
	poses, err := world.CapitalStaticProps(seed)
	if err != nil {
		t.Fatal(err)
	}
	if len(poses) == 0 {
		t.Fatal("empty capture catalogue")
	}
	for i := range poses {
		p := &poses[i]
		x, z := p.Origin[0]-origin[0], p.Origin[2]-origin[2]
		for range turn {
			x, z = 62-z, x
		}
		p.Origin[0], p.Origin[2] = origin[0]+x, origin[2]+z
		p.Facing = (p.Facing-1+uint8(turn))%4 + 1
	}
	index, err := newStaticPropIndex(poses)
	if err != nil {
		t.Fatal(err)
	}
	for coord := range index.chunks {
		index.materialise(coord)
	}
	states := index.visible(propChunk([3]float64{float64(origin[0]) + 31, float64(origin[1]) + 32, float64(origin[2]) + 31}), 8, nil)
	if len(states) != len(poses) || len(states) == 0 {
		t.Fatal("incomplete capture catalogue")
	}
	snapshot := protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{
		Tick: 1, StaticProps: states,
		Vitals: protocol.PlayerVitals{Health: 100, MaxHealth: 100, Hunger: 100, MaxHunger: 100, Energy: 100, MaxEnergy: 100, Level: 1, ExperienceToNext: 100},
	})
	if err := os.WriteFile(path+".snapshot", snapshot, 0600); err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(snapshot)
	fixtureDigest := sha256.Sum256(data)
	metadata, err := json.MarshalIndent(map[string]string{"source_commit": os.Getenv("CASTLE_CAPTURE_SOURCE_COMMIT"), "sha256": hex.EncodeToString(digest[:]), "fixture_sha256": hex.EncodeToString(fixtureDigest[:])}, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path+".snapshot.json", append(metadata, '\n'), 0600); err != nil {
		t.Fatal(err)
	}
}
