package session

import (
	"errors"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	flatbuffers "github.com/google/flatbuffers/go"
	"math"
	"testing"
)

type arrivalResolver struct{ blocked [3]float64 }

func (r arrivalResolver) SafeStaticPropArrival(pos, fallback [3]float64) ([3]float64, error) {
	if pos == r.blocked {
		if fallback == r.blocked {
			return pos, errors.New("blocked fallback")
		}
		return fallback, nil
	}
	return pos, nil
}
func TestStaticPropNormalizationPrecedesWelcomeWithoutMutatingSavedLife(t *testing.T) {
	saved := [3]float64{31.5, 64, 31.5}
	fallback := [3]float32{100.5, 64, 100.5}
	cfg := Config{WorldSeed: 1, TickRate: 20, ChunkSize: 32, ViewDistance: 3, Spawn: fallback}
	life := &game.Life{Pos: saved, Health: 42, Hunger: 80}
	normalizedCfg, self, err := normalizeStaticPropArrival(arrivalResolver{blocked: saved}, cfg, fallback, Resolved{Life: life})
	if err != nil {
		t.Fatal(err)
	}
	if life.Pos != saved || self.Life == life || self.Life.Health != 42 || self.Life.Hunger != 80 {
		t.Fatal("normalization altered saved record or unrelated state")
	}
	envelope := vnet.GetRootAsEnvelope(Welcome(normalizedCfg, 1, self), 0)
	var payload flatbuffers.Table
	if !envelope.Payload(&payload) {
		t.Fatal("welcome payload missing")
	}
	welcome := new(vnet.ServerWelcome)
	welcome.Init(payload.Bytes, payload.Pos)
	pos := welcome.Spawn(nil)
	if [3]float32{pos.X(), pos.Y(), pos.Z()} != fallback {
		t.Fatal("welcome announced obstructed saved position")
	}
	for axis := range 3 {
		if self.Life.Pos[axis] != float64(fallback[axis]) {
			t.Fatal("Join life differs from Welcome")
		}
	}
	// An unobstructed restore preserves the provider's exact position and identity.
	_, same, err := normalizeStaticPropArrival(arrivalResolver{}, cfg, fallback, Resolved{Life: life})
	if err != nil || same.Life != life {
		t.Fatal("unobstructed restore changed")
	}
}

func TestStaticPropNormalizationCannotRepairMalformedSavedLife(t *testing.T) {
	cfg := Config{Spawn: [3]float32{1, 64, 1}}
	life := &game.Life{Pos: [3]float64{math.NaN(), 64, 1}, Health: 42, Hunger: 80}
	if _, _, err := normalizeStaticPropArrival(arrivalResolver{}, cfg, cfg.Spawn, Resolved{Life: life}); err == nil {
		t.Fatal("malformed saved position was normalized instead of rejected")
	}
	if !math.IsNaN(life.Pos[0]) {
		t.Fatal("malformed saved record was mutated")
	}
}

func TestStaticPropConfiguredSpawnFailsClosedButSeparateReturnCanRecover(t *testing.T) {
	blocked := [3]float32{31.5, 64, 31.5}
	cfg := Config{Spawn: blocked}
	resolver := arrivalResolver{blocked: [3]float64{31.5, 64, 31.5}}
	if _, _, err := normalizeStaticPropArrival(resolver, cfg, blocked, Resolved{}); err == nil {
		t.Fatal("blocked configured spawn bypassed collision")
	}
	fallback := [3]float32{100.5, 64, 100.5}
	// welcomeCfg may carry an ephemeral portal return while cfg.Spawn remains the
	// ordinary world spawn. No saved Life exists in that path either.
	normalized, self, err := normalizeStaticPropArrival(resolver, cfg, fallback, Resolved{})
	if err != nil || normalized.Spawn != fallback || self.Life != nil {
		t.Fatal("distinct safe world fallback did not replace blocked return")
	}
}
