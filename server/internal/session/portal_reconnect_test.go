package session_test

import (
	"context"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
	"syscall"
	"testing"
	"time"
)

func TestPortalReconnectHandshakeAndPersistenceBoundary(t *testing.T) {
	for _, mode := range []string{"live", "expired", "restart", "ephemeral", "welcome-write", "transition-write"} {
		t.Run(mode, func(t *testing.T) {
			cfg, chunks, open, peers, _ := portalSession(t, 2)
			identities, store := knownIdentities(t)
			if mode == "ephemeral" {
				store = persist.NewMemoryStore()
				identities = identitiesOver(store)
			}
			var fallback [3]float64
			for axis, value := range cfg.Spawn {
				fallback[axis] = float64(value)
			}
			character := seedAt(t, store, testAccount(91), "Traveller", fallback)
			var failKind vnet.Payload
			start := func() (*fakeConn, chan error) {
				conn := newFakeConn()
				done := make(chan error, 1)
				var wire transport.Conn = conn
				if failKind != 0 {
					wire = &portalWriteFailure{fakeConn: conn, kind: failKind}
				}
				go func() {
					done <- session.Serve(context.Background(), wire, cfg, noTimeouts(), chunks, open, peers, identities, peers.NextID(), discard())
				}()
				conn.in <- hello(91)
				characterList(t, nextFrame(t, conn))
				conn.in <- protocol.EncodeSelectCharacterRequest(protocol.SelectCharacterRequest{CharacterID: uint64(character.ID)})
				return conn, done
			}
			conn, done := start()
			welcomeFrom(t, vnet.GetRootAsEnvelope(nextFrame(t, conn), 0))
			frames := collect(t, conn)
			walkIntoVeil(t, conn, open, "entry", func() bool { return len(frames.transitions()) == 1 })
			change := frames.transitions()[0]
			instance, _ := cfg.Instances.Lookup(change.WorldID)
			// From here on the open-world position this character is kept at is the spot it
			// walked into the veil at, not the spawn it started from.
			seeded := fallback
			fallback = cfg.Instances.Records(open)[game.InstanceCharacter{PlayerID: character.Owner, CharacterID: uint64(character.ID)}].Pos
			if fallback == seeded {
				t.Fatal("fixture: the crossing kept the spawn as its return point")
			}
			conn.in <- protocol.EncodePlayerInput(protocol.PlayerInput{ClientTick: walkClientTicks.Add(1), MoveX: .5})
			conn.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: "walking"})
			waitUntil(t, "movement accepted", func() bool { return len(frames.chatMessages()) == 1 })
			for range 3 {
				cfg.Instances.Step()
			}
			original := instance.Sim.Records()[character.Owner]
			if original.Pos == fallback {
				t.Fatal("fixture never entered instance")
			}
			if err := identities.RememberCharacters(cfg.Instances.Records(open)); err != nil {
				t.Fatal(err)
			}
			saved, found, err := store.Load(character.ID)
			if mode != "ephemeral" && (err != nil || !found || saved.Pos != fallback) {
				t.Fatal("autosave wrote instance coordinates", err)
			}
			_ = conn.Close()
			endsCleanly(t, done)
			if identities.Count() != 0 {
				t.Fatal("teardown retained account claim")
			}
			saved, found, err = store.Load(character.ID)
			if mode != "ephemeral" && (err != nil || !found || saved.Pos != fallback) {
				t.Fatal("teardown wrote instance coordinates", err)
			}
			retained, _ := cfg.Instances.Lookup(change.WorldID)
			if len(retained.Members) != 0 {
				t.Fatal("disconnected member remains inside")
			}
			if mode == "expired" || mode == "ephemeral" {
				for range int(30*time.Minute/time.Second) * int(cfg.TickRate) {
					cfg.Instances.Step()
				}
				if cfg.Instances.Count() != 0 {
					t.Fatal("disconnected member prevented expiry")
				}
			}
			if mode == "ephemeral" {
				cfg.Spawn[0] += 100 // ensure the default spawn cannot masquerade as the arch fallback
			}
			if mode == "restart" {
				cfg.Instances.Close()
				cfg, chunks, open, peers, _ = portalSession(t, 2)
				identities = identitiesOver(store)
			}
			if mode == "welcome-write" {
				failKind = vnet.PayloadServerWelcome
			}
			if mode == "transition-write" {
				failKind = vnet.PayloadWorldChange
			}
			conn, done = start()
			if failKind != 0 {
				endsCleanly(t, done)
				retained, _ := cfg.Instances.Lookup(change.WorldID)
				if len(retained.Members) != 0 || retained.Sim.Count() != 0 || identities.Count() != 0 {
					t.Fatal("failed reconnect retained occupancy or identity")
				}
				failKind = 0
				mode = "live"
				conn, done = start()
			}
			welcome := welcomeFrom(t, vnet.GetRootAsEnvelope(nextFrame(t, conn), 0))
			spawn := welcome.Spawn(nil)
			if spawn == nil || [3]float32{spawn.X(), spawn.Y(), spawn.Z()} != [3]float32{float32(fallback[0]), float32(fallback[1]), float32(fallback[2])} {
				t.Fatal("welcome exposed instance coordinates as open world")
			}
			if mode == "live" {
				frame := nextFrame(t, conn)
				if vnet.GetRootAsEnvelope(frame, 0).PayloadType() != vnet.PayloadWorldChange {
					t.Fatal("Welcome was not followed by WorldChange")
				}
				var transition collector
				transition.absorb(frame)
				got := transition.transitions()[0]
				arrival := [3]float32{float32(original.Pos[0]), float32(original.Pos[1]), float32(original.Pos[2])}
				if got.WorldID != change.WorldID || got.Arrival != arrival || got.ExitArch != change.ExitArch {
					t.Fatal("wrong reconnect transition")
				}
				waitUntil(t, "instance rejoined", func() bool { return instance.Sim.Count() == 1 })
				if open.Count() != 0 || instance.Sim.Records()[character.Owner] != original {
					t.Fatal("reconnect lost exact authoritative life")
				}
			} else {
				waitUntil(t, "open world rejoined", func() bool { return open.Count() == 1 })
				if open.Records()[character.Owner].Pos != fallback {
					t.Fatal("expired/cold reconnect used foreign coordinates")
				}
			}
			resumed := collect(t, conn)
			if mode != "live" {
				conn.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: "back"})
				waitUntil(t, "open chat", func() bool { return len(resumed.chatMessages()) == 1 })
				if len(resumed.transitions()) != 0 {
					t.Fatal("dead instance announced")
				}
			}
			_ = conn.Close()
			endsCleanly(t, done)
		})
	}
}

// Failure happens only at the named reconnect boundary; the character exchange
// still succeeds, so this exercises the writer barrier with an already failed writer.
type portalWriteFailure struct {
	*fakeConn
	kind vnet.Payload
}

func (c *portalWriteFailure) WriteFrame(frame []byte) error {
	if vnet.GetRootAsEnvelope(frame, 0).PayloadType() == c.kind {
		return syscall.EPIPE
	}
	return c.fakeConn.WriteFrame(frame)
}

func TestExternalWorldBindingWithoutPortalDoesNotPersistForeignPosition(t *testing.T) {
	cfg, chunks, open, peers, _ := portalSession(t, 1)
	identities, store := knownIdentities(t)
	var fallback [3]float64
	for axis, value := range cfg.Spawn {
		fallback[axis] = float64(value)
	}
	character := seedAt(t, store, testAccount(92), "Traveller", fallback)
	conn := newFakeConn()
	_ = collect(t, conn)
	done := make(chan error, 1)
	entity := peers.NextID()
	go func() {
		done <- session.Serve(context.Background(), conn, cfg, noTimeouts(), chunks, open, peers, identities, entity, discard())
	}()
	conn.in <- hello(92)
	conn.in <- protocol.EncodeSelectCharacterRequest(protocol.SelectCharacterRequest{CharacterID: uint64(character.ID)})
	waitUntil(t, "open world joined", func() bool { return open.Count() == 1 })
	instance, err := cfg.Instances.Create(game.InstanceRuin{CellX: 1, CellZ: 2})
	if err != nil {
		t.Fatal(err)
	}
	// Controls are installed after the initial inventory/map sends. Retry only
	// the readiness lookup, not a successful transition.
	var moved bool
	waitUntil(t, "external binding ready", func() bool {
		err := peers.ChangeWorld(context.Background(), entity, session.WorldBinding{Chunks: instance.Chunks, Sim: instance.Sim, Context: instance.Context, Spawn: [3]float32{.5, 2, .5}})
		moved = err == nil
		return moved
	})
	if !moved || instance.Sim.Count() != 1 {
		t.Fatal("fixture did not transfer")
	}
	_ = conn.Close()
	endsCleanly(t, done)
	saved, found, err := store.Load(character.ID)
	if err != nil || !found || saved.Pos != fallback {
		t.Fatal("external binding persisted foreign coordinates", err)
	}
}
