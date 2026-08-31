package main

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

type onePassClock struct{ now time.Time }

func (c onePassClock) Now() time.Time { return c.now }
func (onePassClock) SleepUntil(context.Context, time.Time) error {
	return context.Canceled
}

type stalledStormClock struct {
	now       time.Time
	deadlines []time.Time
}

func (c *stalledStormClock) Now() time.Time { return c.now }

func (c *stalledStormClock) SleepUntil(_ context.Context, deadline time.Time) error {
	c.deadlines = append(c.deadlines, deadline)
	if len(c.deadlines) == 1 {
		c.now = deadline.Add(time.Hour)
		return nil
	}
	return context.Canceled
}

type warningRecorder struct {
	mu       sync.Mutex
	warnings []protocol.StormWarning
}

func (r *warningRecorder) deliver(frame []byte) bool {
	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadStormWarning {
		return true
	}
	table := new(flatbuffers.Table)
	if !env.Payload(table) {
		return true
	}
	warning := new(vnet.StormWarning)
	warning.Init(table.Bytes, table.Pos)
	r.mu.Lock()
	r.warnings = append(r.warnings, protocol.StormWarning{
		Phase: warning.Phase(), SecondsUntil: warning.SecondsUntil(),
	})
	r.mu.Unlock()
	return true
}

func (r *warningRecorder) snapshot() []protocol.StormWarning {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]protocol.StormWarning(nil), r.warnings...)
}

func newStormHarness(t *testing.T) (*server, *warningRecorder) {
	t.Helper()
	chunks := world.NewCache(1, 1, 16)
	registry := session.NewRegistry(session.DefaultConcurrentSessions)
	sim, err := game.NewSim(game.DefaultTickRate, 1, 1, game.NewCacheTerrain(chunks), chunks,
		registry.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	if err := sim.ConfigureChunkRegeneration(chunks, registry.ResendChunk); err != nil {
		t.Fatalf("ConfigureChunkRegeneration: %v", err)
	}
	recorder := new(warningRecorder)
	if _, err := sim.Join(1, testPlayerID(testAccount(1)), "Eivor", world.SpawnAt(1),
		testAppearance(), nil, recorder.deliver); err != nil {
		t.Fatalf("Join: %v", err)
	}
	return &server{
		chunks: chunks, clock: openClockStore(t, t.TempDir()), sim: sim,
		stormPeriod: game.DefaultStormPeriod, log: discard(),
	}, recorder
}

func TestStormPassSchedulesWarnsRagesPassesAndPersists(t *testing.T) {
	t.Parallel()

	srv, recorder := newStormHarness(t)
	start := time.Unix(1_800_000_000, 0)
	srv.wallClock = onePassClock{now: start}
	if err := srv.stormLoop(context.Background()); !errors.Is(err, context.Canceled) {
		t.Fatalf("one-pass fake clock stopped with %v", err)
	}
	due := start.Unix() + int64(game.DefaultStormPeriod/time.Second)
	if got := srv.sim.NextStorm(); got != due {
		t.Fatalf("first deadline = %d, want %d", got, due)
	}
	stored, found, err := srv.clock.Load()
	if err != nil || !found || stored.NextStormUnix != due {
		t.Fatalf("stored first deadline = (%+v, %v, %v)", stored, found, err)
	}

	for _, before := range []time.Duration{10 * time.Minute, time.Minute, 10 * time.Second, 0} {
		srv.stormPass(time.Unix(due, 0).Add(-before))
	}
	want := []protocol.StormWarning{
		{Phase: vnet.StormPhaseApproaching, SecondsUntil: 600},
		{Phase: vnet.StormPhaseApproaching, SecondsUntil: 60},
		{Phase: vnet.StormPhaseApproaching, SecondsUntil: 10},
		{Phase: vnet.StormPhaseRaging, SecondsUntil: 300},
	}
	got := recorder.snapshot()
	if len(got) != len(want) {
		t.Fatalf("phase broadcasts = %+v, want %+v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("broadcast %d = %+v, want %+v", i, got[i], want[i])
		}
	}

	srv.stormPass(time.Unix(due, 0).Add(game.StormDuration - time.Second))
	if warning, active := srv.sim.StormWarning(); !active || warning.SecondsUntil != 1 {
		t.Fatalf("last raging second = (%+v, %v)", warning, active)
	}
	if _, _, err := srv.chunks.Get(context.Background(), world.Coord{X: 100}); err != nil {
		t.Fatalf("make a regeneration candidate resident: %v", err)
	}
	srv.stormPass(time.Unix(due, 0).Add(game.StormDuration))
	if got := srv.sim.NextStorm(); got != due {
		t.Fatalf("deadline advanced to %d before regeneration, want original %d", got, due)
	}
	if warning, active := srv.sim.StormWarning(); !active || warning.Phase != vnet.StormPhaseRaging {
		t.Fatalf("phase while regeneration is queued = (%+v, %v), want Raging", warning, active)
	}
	stored, found, err = srv.clock.Load()
	if err != nil || !found || stored.NextStormUnix != due {
		t.Fatalf("stored deadline before regeneration = (%+v, %v, %v), want %d", stored, found, err, due)
	}
	restarted, _ := newStormHarness(t)
	restarted.sim.ScheduleStorm(stored.NextStormUnix)
	restarted.stormPass(time.Unix(due, 0).Add(game.StormDuration + time.Second))
	if warning, active := restarted.sim.StormWarning(); !active ||
		warning.Phase != vnet.StormPhaseApproaching || warning.SecondsUntil != 60 {
		t.Fatalf("phase after a crash with queued healing = (%+v, %v), want Approaching 60", warning, active)
	}

	srv.sim.Step(1)
	srv.stormPass(time.Unix(due, 0).Add(game.StormDuration + stormPollInterval))
	got = recorder.snapshot()
	last := got[len(got)-1]
	if last.Phase != vnet.StormPhasePassed || last.SecondsUntil != 0 {
		t.Fatalf("last broadcast = %+v, want Passed 0", last)
	}
	next := due + int64(game.DefaultStormPeriod/time.Second)
	if srv.sim.NextStorm() != next {
		t.Fatalf("next deadline = %d, want %d", srv.sim.NextStorm(), next)
	}
	stored, found, err = srv.clock.Load()
	if err != nil || !found || stored.NextStormUnix != next {
		t.Fatalf("stored next deadline = (%+v, %v, %v)", stored, found, err)
	}
}

func TestAStalledStormPollerCoalescesMissedIntervals(t *testing.T) {
	t.Parallel()

	srv, _ := newStormHarness(t)
	start := time.Unix(1_800_000_000, 0)
	clock := &stalledStormClock{now: start}
	srv.wallClock = clock
	if err := srv.stormLoop(context.Background()); !errors.Is(err, context.Canceled) {
		t.Fatalf("stormLoop after fake stall = %v, want context.Canceled", err)
	}
	if len(clock.deadlines) != 2 {
		t.Fatalf("sleep deadlines = %v, want two", clock.deadlines)
	}
	if got, want := clock.deadlines[1], clock.now.Add(stormPollInterval); !got.Equal(want) {
		t.Fatalf("deadline after stall = %v, want coalesced %v", got, want)
	}
}

func TestZeroStormPeriodDisablesAStoredSchedule(t *testing.T) {
	t.Parallel()

	srv, _ := newStormHarness(t)
	srv.sim.ScheduleStorm(1_800_000_000)
	srv.sim.BeginStorm(120)
	srv.stormPeriod = 0
	if err := srv.stormLoop(context.Background()); err != nil {
		t.Fatalf("disabled storm loop: %v", err)
	}
	if got := srv.sim.NextStorm(); got != 0 {
		t.Fatalf("disabled storm retained deadline %d", got)
	}
	if warning, active := srv.sim.StormWarning(); active {
		t.Fatalf("disabled storm retained phase %+v", warning)
	}
}

func TestStormPassResumesRagingAndTurnsAMissedDeadlineIntoAOneMinuteWarning(t *testing.T) {
	t.Parallel()

	now := time.Unix(1_800_100_000, 0)
	resumed, _ := newStormHarness(t)
	resumed.sim.ScheduleStorm(now.Add(-2 * time.Minute).Unix())
	resumed.stormPass(now)
	if warning, active := resumed.sim.StormWarning(); !active ||
		warning.Phase != vnet.StormPhaseRaging || warning.SecondsUntil != 180 {
		t.Fatalf("resumed phase = (%+v, %v), want Raging 180", warning, active)
	}

	missed, recorder := newStormHarness(t)
	missed.sim.ScheduleStorm(now.Add(-game.StormDuration - time.Second).Unix())
	missed.stormPass(now)
	warning, active := missed.sim.StormWarning()
	if !active || warning.Phase != vnet.StormPhaseApproaching || warning.SecondsUntil != 60 {
		t.Fatalf("missed phase = (%+v, %v), want Approaching 60", warning, active)
	}
	if got := missed.sim.NextStorm(); got != now.Add(time.Minute).Unix() {
		t.Fatalf("replacement deadline = %d, want %d", got, now.Add(time.Minute).Unix())
	}
	got := recorder.snapshot()
	if len(got) != 1 || got[0] != warning {
		t.Fatalf("missed-deadline broadcasts = %+v, want [%+v]", got, warning)
	}
}

func TestAJoiningPlayerReceivesTheLiveStormPhaseAfterTheWelcome(t *testing.T) {
	for name, offset := range map[string]time.Duration{
		"approaching": 5 * time.Minute,
		"raging":      -time.Minute,
	} {
		t.Run(name, func(t *testing.T) {
			tr := newQueueTransport()
			srv := testServer(t, tr)
			srv.stormPeriod = time.Hour
			srv.sim.ScheduleStorm(time.Now().Add(offset).Unix())
			stop := start(t, srv)
			defer stop()

			waitFor(t, "the storm phase to settle", func() bool {
				_, active := srv.sim.StormWarning()
				return active
			})
			conn := newScriptedConn(name)
			tr.conns <- conn
			welcome := enterWorld(t, conn, helloFor(t, testAccount(3)), creationOf("Eivor"))
			if welcome.PayloadType() != vnet.PayloadServerWelcome {
				t.Fatalf("choice answer = %s, want welcome", welcome.PayloadType())
			}
			phase := nextReply(t, conn)
			if phase.PayloadType() != vnet.PayloadStormWarning {
				t.Fatalf("frame after welcome = %s, want storm warning", phase.PayloadType())
			}
			table := new(flatbuffers.Table)
			if !phase.Payload(table) {
				t.Fatal("storm warning payload was absent")
			}
			warning := new(vnet.StormWarning)
			warning.Init(table.Bytes, table.Pos)
			wantPhase := vnet.StormPhaseApproaching
			if offset < 0 {
				wantPhase = vnet.StormPhaseRaging
			}
			if warning.Phase() != wantPhase || warning.SecondsUntil() == 0 {
				t.Fatalf("join warning = %s %d, want %s with time remaining",
					warning.Phase(), warning.SecondsUntil(), wantPhase)
			}
		})
	}
}
