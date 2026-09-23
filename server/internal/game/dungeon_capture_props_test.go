package game

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"os"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// TestExportDungeonCaptureSnapshot writes the dungeon's wall sconces beside a VHDUNG01
// fixture (world's TestExportDungeonCaptureFixture) as the snapshot a session in that
// instance is sent: the production placement (world.InstanceStaticProps), the production
// static-prop index and the production wire encoder. The client's capture lights the cave
// from it exactly as a session's castle_lighting would, and from nothing else.
func TestExportDungeonCaptureSnapshot(t *testing.T) {
	path := os.Getenv("DUNGEON_CAPTURE_FIXTURE")
	if path == "" {
		t.Skip("opt-in dungeon snapshot export")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(data) < 96 || string(data[:8]) != "VHDUNG01" {
		t.Fatal("invalid fixture")
	}
	commit := os.Getenv("DUNGEON_CAPTURE_SOURCE_COMMIT")
	if decoded, err := hex.DecodeString(commit); err != nil || len(decoded) != 20 {
		t.Fatal("full source commit required")
	}
	var sidecar struct {
		SourceCommit string `json:"source_commit"`
		SHA256       string `json:"sha256"`
	}
	raw, err := os.ReadFile(path + ".json")
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(raw, &sidecar); err != nil {
		t.Fatal(err)
	}
	fixtureDigest := sha256.Sum256(data)
	if sidecar.SourceCommit != commit || sidecar.SHA256 != hex.EncodeToString(fixtureDigest[:]) {
		t.Fatal("fixture provenance mismatch")
	}
	seed := int64(binary.LittleEndian.Uint64(data[16:24]))
	poses, err := world.InstanceStaticProps(seed)
	if err != nil {
		t.Fatal(err)
	}
	index, err := newStaticPropIndex(poses)
	if err != nil || index == nil {
		t.Fatal("the dungeon has no sconces to export")
	}
	// Every sconce, whatever chunk it is in: the capture moves its camera through every
	// zone, and the client's bounded light pool picks the nearest itself.
	var states []protocol.StaticPropState
	for i := range index.props {
		states = append(states, index.props[i].state)
	}
	if len(states) != len(poses) {
		t.Fatal("incomplete capture catalogue")
	}
	snapshot := protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{
		Tick: 1, StaticProps: states,
		Vitals: protocol.PlayerVitals{LifeState: vnet.LifeStateAlive, Health: 100, MaxHealth: 100, Hunger: 100, MaxHunger: 100, Energy: 100, MaxEnergy: 100, Level: 1, ExperienceToNext: 100},
	})
	if err := protocol.ValidateEntitySnapshot(snapshot); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path+".snapshot", snapshot, 0600); err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(snapshot)
	metadata, err := json.MarshalIndent(map[string]string{"source_commit": commit, "sha256": hex.EncodeToString(digest[:]), "fixture_sha256": hex.EncodeToString(fixtureDigest[:])}, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path+".snapshot.json", append(metadata, '\n'), 0600); err != nil {
		t.Fatal(err)
	}
}
