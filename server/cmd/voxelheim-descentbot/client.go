package main

import (
	"bufio"
	"context"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"errors"
	"fmt"
	"math"
	"net"
	"sync"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// One session, built from the packages the server admits — internal/protocol's encoders,
// internal/transport's framing and internal/ticket's minting — as the voice soak bot is.
// What the bot knows about the world is exactly what arrives on this socket.

// mobView is one creature as the latest snapshot showed it.
type mobView struct {
	id        uint64
	kind      vnet.MobKind
	pos       [3]float64
	health    uint16
	maxHealth uint16
	action    vnet.MobAction
	target    uint64
	seen      time.Time
}

func (m mobView) targetID() uint64 { return m.target }

func (m mobView) dying() bool {
	return m.health == 0 || m.action == vnet.MobActionDying || m.action == vnet.MobActionCorpse
}

// intent is what the controls are doing, sent every tick as PlayerInput.
type intent struct {
	moveX, moveZ float64
	yaw, pitch   float64
	jump         bool
}

// worldChange is one arrival in a world, open or instanced.
type worldChange struct {
	id      uint64
	seed    int64
	arrival [3]float64
}

type client struct {
	name     string
	conn     net.Conn
	reader   *bufio.Reader
	entityID uint64

	writeMu    sync.Mutex
	clientTick uint32

	mu        sync.Mutex
	view      *blockView
	worldID   uint64
	worldSeed int64
	pos       [3]float64
	havePos   bool
	health    uint16
	maxHealth uint16
	alive     bool
	level     uint16
	energy    uint16
	mobs      map[uint64]mobView
	control   intent
	// updates counts every BlockUpdate by cell, so a caller can wait for "this cell
	// changed since I asked" without keeping its own subscription.
	updates  map[cell]int
	stacks   []uint16
	chunksIn int

	// The facts the report is built from, written by the reader.
	stats *runStats

	changes  chan worldChange
	offers   chan uint64
	chat     chan string
	refusals chan string
}

func dial(ctx context.Context, addr, fingerprint string) (net.Conn, error) {
	config := &tls.Config{
		InsecureSkipVerify: true, //nolint:gosec // the fingerprint pin below is the check.
		MinVersion:         tls.VersionTLS13,
		VerifyPeerCertificate: func(raw [][]byte, _ [][]*x509.Certificate) error {
			if len(raw) == 0 {
				return errors.New("the server presented no certificate")
			}
			sum := sha256.Sum256(raw[0])
			if hex.EncodeToString(sum[:]) != fingerprint {
				return errors.New("the server's certificate is not the one it announced")
			}
			return nil
		},
	}
	dialer := &tls.Dialer{NetDialer: &net.Dialer{Timeout: 30 * time.Second}, Config: config}
	return dialer.DialContext(ctx, "tcp", addr)
}

// join performs the handshake: hello with a ticket and a character creation written back
// to back, then everything up to the welcome.
func join(ctx context.Context, addr, fingerprint, name string, credential []byte, stats *runStats) (*client, error) {
	conn, err := dial(ctx, addr, fingerprint)
	if err != nil {
		return nil, fmt.Errorf("dial: %w", err)
	}
	c := &client{
		name: name, conn: conn, reader: bufio.NewReaderSize(conn, 64<<10),
		view: newBlockView(), mobs: make(map[uint64]mobView), updates: make(map[cell]int),
		stats: stats, alive: true,
		changes: make(chan worldChange, 4), offers: make(chan uint64, 4),
		chat: make(chan string, 64), refusals: make(chan string, 64),
	}
	if err := conn.SetDeadline(time.Now().Add(30 * time.Second)); err != nil {
		return nil, err
	}
	hello := protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, name, credential)
	create := protocol.EncodeCreateCharacterRequest(protocol.CreateCharacterRequest{
		Name: name, Appearance: protocol.Appearance{
			SkinColor: 0x00E3C4A0, ShirtColor: 0x004A5D3B, TrousersColor: 0x002B2118,
			ShoesColor: 0x00553311, HairModel: vnet.HairModelBraided, HairColor: 0x00B07A32,
		}, HasAppearance: true,
	})
	for _, frame := range [][]byte{hello, create} {
		if err := transport.WriteFrame(conn, frame); err != nil {
			return nil, fmt.Errorf("write the handshake: %w", err)
		}
	}
	for {
		frame, err := transport.ReadFrame(c.reader)
		if err != nil {
			return nil, fmt.Errorf("read the handshake: %w", err)
		}
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		var table flatbuffers.Table
		switch envelope.PayloadType() {
		case vnet.PayloadServerWelcome:
			if !envelope.Payload(&table) {
				return nil, errors.New("the welcome carried no payload")
			}
			var welcome vnet.ServerWelcome
			welcome.Init(table.Bytes, table.Pos)
			c.entityID = welcome.EntityId()
			c.worldSeed = welcome.WorldSeed()
			return c, conn.SetDeadline(time.Time{})
		case vnet.PayloadServerReject:
			if !envelope.Payload(&table) {
				return nil, errors.New("refused without a reason")
			}
			var reject vnet.ServerReject
			reject.Init(table.Bytes, table.Pos)
			return nil, fmt.Errorf("refused: %s", reject.Detail())
		}
	}
}

// listen is the read half. It runs until the connection ends.
func (c *client) listen(ctx context.Context) error {
	stop := context.AfterFunc(ctx, func() { _ = c.conn.SetDeadline(time.Now()) })
	defer stop()
	for {
		frame, err := transport.ReadFrame(c.reader)
		if err != nil {
			if ctx.Err() != nil || transport.IsDisconnect(err) {
				return nil
			}
			return err
		}
		c.absorb(vnet.GetRootAsEnvelope(frame, 0))
	}
}

func (c *client) absorb(envelope *vnet.Envelope) {
	var table flatbuffers.Table
	if !envelope.Payload(&table) {
		return
	}
	switch envelope.PayloadType() {
	case vnet.PayloadChunkData:
		var data vnet.ChunkData
		data.Init(table.Bytes, table.Pos)
		var coord vnet.ChunkCoord
		data.Coord(&coord)
		runs := make([]uint16, data.RunsLength())
		for i := range runs {
			runs[i] = data.Runs(i)
		}
		blocks, err := world.Decode(runs)
		if err != nil {
			return
		}
		c.mu.Lock()
		c.view.chunks[world.Coord{X: coord.Cx(), Y: coord.Cy(), Z: coord.Cz()}] = blocks
		c.chunksIn++
		c.mu.Unlock()
	case vnet.PayloadChunkUnload:
		var unload vnet.ChunkUnload
		unload.Init(table.Bytes, table.Pos)
		var coord vnet.ChunkCoord
		unload.Coord(&coord)
		c.mu.Lock()
		delete(c.view.chunks, world.Coord{X: coord.Cx(), Y: coord.Cy(), Z: coord.Cz()})
		c.mu.Unlock()
	case vnet.PayloadBlockUpdate:
		var update vnet.BlockUpdate
		update.Init(table.Bytes, table.Pos)
		var pos vnet.BlockCoord
		update.Pos(&pos)
		at := cell{int64(pos.X()), int64(pos.Y()), int64(pos.Z())}
		c.mu.Lock()
		c.view.set(at[0], at[1], at[2], world.Block(update.BlockId()))
		c.updates[at]++
		c.mu.Unlock()
	case vnet.PayloadEntitySnapshot:
		c.absorbSnapshot(table)
	case vnet.PayloadWorldChange:
		var change vnet.WorldChange
		change.Init(table.Bytes, table.Pos)
		var arrival vnet.Vec3
		change.Arrival(&arrival)
		wc := worldChange{id: change.WorldId(), seed: change.WorldSeed(),
			arrival: [3]float64{float64(arrival.X()), float64(arrival.Y()), float64(arrival.Z())}}
		c.mu.Lock()
		// Another world: nothing the bot knew about the last one is true here.
		c.view = newBlockView()
		c.mobs = make(map[uint64]mobView)
		c.worldID, c.worldSeed, c.havePos = wc.id, wc.seed, false
		c.mu.Unlock()
		select {
		case c.changes <- wc:
		default:
		}
	case vnet.PayloadInstanceEntryOffer:
		var offer vnet.InstanceEntryOffer
		offer.Init(table.Bytes, table.Pos)
		select {
		case c.offers <- offer.OfferId():
		default:
		}
	case vnet.PayloadChatMessage:
		var message vnet.ChatMessage
		message.Init(table.Bytes, table.Pos)
		select {
		case c.chat <- string(message.Text()):
		default:
		}
	case vnet.PayloadActionRefused:
		var refused vnet.ActionRefused
		refused.Init(table.Bytes, table.Pos)
		select {
		case c.refusals <- fmt.Sprintf("%s: %s", vnet.EnumNamesRefusedAction[refused.Action()], vnet.EnumNamesRefusalReason[refused.Reason()]):
		default:
		}
	case vnet.PayloadInventoryState:
		var state vnet.InventoryState
		state.Init(table.Bytes, table.Pos)
		stacks := make([]uint16, state.StacksLength())
		for i := range stacks {
			stacks[i] = state.Stacks(i)
		}
		c.mu.Lock()
		c.stacks = stacks
		c.mu.Unlock()
	case vnet.PayloadBlowLanded:
		var blow vnet.BlowLanded
		blow.Init(table.Bytes, table.Pos)
		if blow.AttackerEntityId() == c.entityID && blow.Target() == vnet.BlowTargetMob {
			c.stats.blowLanded(blow.TargetEntityId())
		}
		if blow.TargetEntityId() == c.entityID && blow.Target() == vnet.BlowTargetPlayer {
			c.stats.blowTaken()
		}
	}
}

func (c *client) absorbSnapshot(table flatbuffers.Table) {
	var snapshot vnet.EntitySnapshot
	snapshot.Init(table.Bytes, table.Pos)
	now := time.Now()
	c.mu.Lock()
	defer c.mu.Unlock()
	var state vnet.EntityState
	for i := range snapshot.EntitiesLength() {
		if snapshot.Entities(&state, i) && state.EntityId() == c.entityID {
			p := state.Pos(nil)
			c.pos = [3]float64{float64(p.X()), float64(p.Y()), float64(p.Z())}
			c.havePos = true
		}
	}
	if vitals := snapshot.SelfVitals(nil); vitals != nil {
		alive := vitals.LifeState() != vnet.LifeStateDead
		if c.alive && !alive {
			c.stats.died()
		}
		c.alive, c.health, c.maxHealth = alive, vitals.Health(), vitals.MaxHealth()
		c.level, c.energy = vitals.Level(), vitals.Energy()
	}
	seen := make(map[uint64]bool, snapshot.MobsLength())
	var m vnet.MobState
	for i := range snapshot.MobsLength() {
		if !snapshot.Mobs(&m, i) {
			continue
		}
		p := m.Pos(nil)
		view := mobView{
			id: m.EntityId(), kind: m.Kind(), pos: [3]float64{float64(p.X()), float64(p.Y()), float64(p.Z())},
			health: m.Health(), maxHealth: m.MaxHealth(), action: m.Action(), target: m.TargetEntityId(), seen: now,
		}
		seen[view.id] = true
		if c.worldID != 0 {
			// Only the dungeon's creatures are the run's; the open world's are scenery.
			c.stats.sawMob(view)
		}
		c.mobs[view.id] = view
	}
	// A creature the snapshot no longer carries is either dead and gone or out of view;
	// the stats decide which from whether this bot's blows were the last thing to reach it.
	for id, old := range c.mobs {
		if !seen[id] {
			c.stats.lostMob(old, c.alive)
			delete(c.mobs, id)
		}
	}
}

// send writes one frame; the write half is shared by the heartbeat and the route.
func (c *client) send(frame []byte) error {
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	return transport.WriteFrame(c.conn, frame)
}

// tick is a fresh client tick. Movement, mining, attacks and mechanism uses each have
// their own ordering guard on the server; one shared counter is increasing for all four.
func (c *client) tick() uint32 {
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	c.clientTick++
	return c.clientTick
}

// drive sends the current intent every tick until ctx ends. A welcomed session that says
// nothing is closed for idleness, and PlayerInput is what the real client sends each tick.
func (c *client) drive(ctx context.Context, rate int) {
	ticker := time.NewTicker(time.Second / time.Duration(rate))
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			c.mu.Lock()
			in := c.control
			c.mu.Unlock()
			_ = c.send(protocol.EncodePlayerInput(protocol.PlayerInput{
				ClientTick: c.tick(), MoveX: float32(in.moveX), MoveZ: float32(in.moveZ),
				Yaw: float32(in.yaw), Pitch: float32(in.pitch), Jump: in.jump,
			}))
		}
	}
}

func (c *client) setIntent(in intent) {
	c.mu.Lock()
	c.control = in
	c.mu.Unlock()
}

func (c *client) stand() {
	c.mu.Lock()
	c.control.moveX, c.control.moveZ, c.control.jump = 0, 0, false
	c.mu.Unlock()
}

// command sends one development command and waits for the server's private answer.
func (c *client) command(ctx context.Context, line string) (string, error) {
	drain(c.chat)
	c.stats.command(line)
	if err := c.send(protocol.EncodeChatRequest(protocol.ChatRequest{Text: line})); err != nil {
		return "", err
	}
	timer := time.NewTimer(5 * time.Second)
	defer timer.Stop()
	select {
	case answer := <-c.chat:
		return answer, nil
	case <-timer.C:
		return "", fmt.Errorf("no answer to %q", line)
	case <-ctx.Done():
		return "", ctx.Err()
	}
}

// snapshot copies what the route reads each step.
type selfState struct {
	pos    [3]float64
	have   bool
	alive  bool
	health uint16
	energy uint16
	level  uint16
	world  uint64
	seed   int64
}

func (c *client) self() selfState {
	c.mu.Lock()
	defer c.mu.Unlock()
	return selfState{pos: c.pos, have: c.havePos, alive: c.alive, health: c.health, energy: c.energy,
		level: c.level, world: c.worldID, seed: c.worldSeed}
}

// feetCell is the cell a body's feet stand in: the floor of its position, nudged up a
// hair so a body resting exactly on a block's top is in the cell above it.
func feetCell(pos [3]float64) cell {
	return cell{int64(math.Floor(pos[0])), int64(math.Floor(pos[1] + 0.01)), int64(math.Floor(pos[2]))}
}

func (c *client) mobList() []mobView {
	c.mu.Lock()
	defer c.mu.Unlock()
	out := make([]mobView, 0, len(c.mobs))
	for _, m := range c.mobs {
		out = append(out, m)
	}
	return out
}

func (c *client) updateCount(at cell) int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.updates[at]
}

func (c *client) blockAt(at cell) (world.Block, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.view.block(at[0], at[1], at[2])
}

// withView runs f over the bot's terrain under the lock the reader writes it under.
func (c *client) withView(f func(v *blockView)) {
	c.mu.Lock()
	defer c.mu.Unlock()
	f(c.view)
}
