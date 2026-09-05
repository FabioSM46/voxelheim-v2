package main

import (
	"context"
	"fmt"
	"io"
	"os"
	"sync"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// The run: connect the plan, let it settle, measure a window, and report.
//
// **The measured window starts after everybody has joined and stopped moving**, and that is
// not a convenience. The audible sets are recomputed every game.VoiceSetInterval ticks from
// the positions the tick has just produced, so a frame sent before the recompute that
// follows a teleport reaches an audience that no longer describes where anybody is. Counted
// in, those frames would look like drops.

// joinConcurrency is how many sessions are handshaking at once.
//
// A thousand simultaneous TLS handshakes would queue behind each other inside the server's
// five-second handshake window and be refused for being slow, which would be this
// command's fault reported as the server's.
const joinConcurrency = 32

// drainAfterWindow is how long the run keeps reading after the window closes, so that the
// frames sent at its trailing edge are counted where they landed rather than as drops. It
// is two orders of magnitude past the relay latency this command has ever measured, and it
// costs nothing but half a second of a run that lasts thirty.
const drainAfterWindow = 500 * time.Millisecond

// runSoak performs the whole measurement and writes the report.
func runSoak(ctx context.Context, o options, run runOptions, out io.Writer) error {
	keyDir, err := makeKeyDir()
	if err != nil {
		return err
	}
	defer removeKeyDir(keyDir)

	pair, err := ticket.LoadOrCreate(keyDir)
	if err != nil {
		return fmt.Errorf("mint a signing key: %w", err)
	}
	run.ticketKey = pair.PublicHex()

	worldID, err := ticket.WorldIDFor(o.worldName)
	if err != nil {
		return fmt.Errorf("world name %q: %w", o.worldName, err)
	}

	template, err := silenceFrame(o.opusBytes)
	if err != nil {
		return err
	}

	server, err := startServer(ctx, o, run)
	if err != nil {
		return err
	}
	defer server.stop()

	f := &fleet{
		opts:         o,
		pair:         pair,
		world:        worldID,
		addr:         server.addr,
		fingerprint:  server.fingerprint,
		template:     template,
		tickInterval: time.Second / time.Duration(run.tickRate),
	}

	bots, err := joinFleet(ctx, f)
	if err != nil {
		return err
	}
	defer func() {
		for _, b := range bots {
			b.close()
		}
	}()

	sessions, cancelSessions := context.WithCancel(ctx)
	defer cancelSessions()

	var running sync.WaitGroup
	readErrors := make(chan error, len(bots))
	for _, b := range bots {
		running.Add(2)
		go func() {
			defer running.Done()
			if err := b.listen(sessions); err != nil {
				readErrors <- err
			}
		}()
		go func() {
			defer running.Done()
			b.speak(sessions)
		}()
	}

	for _, b := range bots {
		if err := b.teleport(); err != nil {
			return fmt.Errorf("place %s: %w", b.name, err)
		}
	}

	if err := sleepFor(ctx, run.settle); err != nil {
		return err
	}

	report := soakReport{options: o, run: run, commandLine: server.commandLine, sessions: len(bots)}
	sampler := startSampler(sessions, server.pid(), run.sampleEvery)
	// The load generator is sampled too, so a report can say whether the server or this
	// command was the thing that ran out of processor. A soak test that was itself the
	// bottleneck measures itself, and nothing in the numbers says so.
	self := startSampler(sessions, os.Getpid(), run.sampleEvery)

	windowStart := time.Now()
	f.windowStart.Store(windowStart.UnixNano())
	f.windowEnd.Store(windowStart.Add(o.duration).UnixNano())
	f.measuring.Store(true)

	err = sleepFor(ctx, o.duration)
	report.window = time.Since(windowStart)
	if err == nil {
		// The drain. Every frame sent inside the window is counted by its sender the
		// moment it goes out and by its listeners whenever it lands, so the run has to
		// stay up long enough for the last of them to land; without it the trailing edge
		// would be reported as a drop.
		err = sleepFor(ctx, drainAfterWindow)
	}
	f.measuring.Store(false)
	if err != nil {
		return err
	}

	report.server = sampler.stop()
	report.bot = self.stop()
	report.absorb(bots)
	report.absorbServerLog(server)
	cancelSessions()
	running.Wait()
	close(readErrors)
	for readErr := range readErrors {
		report.readErrors = append(report.readErrors, readErr.Error())
	}
	report.userCPU, report.systemCPU = server.stop()

	return report.render(out)
}

// joinFleet connects every session in the plan and returns them in plan order.
func joinFleet(ctx context.Context, f *fleet) ([]*bot, error) {
	places := planPlacements(f.opts)
	bots := make([]*bot, len(places))
	for i, place := range places {
		bots[i] = &bot{
			fleet:  f,
			place:  place,
			name:   fmt.Sprintf("soak-%05d", i),
			joined: make(chan struct{}),
		}
	}

	var (
		mu      sync.Mutex
		failure error
	)
	gate := make(chan struct{}, joinConcurrency)
	var joining sync.WaitGroup
	for _, b := range bots {
		joining.Add(1)
		go func() {
			defer joining.Done()
			gate <- struct{}{}
			defer func() { <-gate }()

			if err := b.join(ctx); err != nil {
				mu.Lock()
				if failure == nil {
					failure = fmt.Errorf("%s could not join: %w", b.name, err)
				}
				mu.Unlock()
			}
		}()
	}
	joining.Wait()

	if failure != nil {
		for _, b := range bots {
			b.close()
		}
		return nil, failure
	}
	return bots, nil
}

// sleepFor waits, and answers the context rather than the clock when both are ready.
func sleepFor(ctx context.Context, d time.Duration) error {
	if ctx.Err() != nil {
		return ctx.Err()
	}
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return nil
	}
}

// expectedDeliveries is how many receipts a run's frames should produce if nothing is
// dropped anywhere.
//
// Arithmetic rather than observation, and that is the point: a speaker's audible set is
// every other member of its own cluster — options.validate is what makes that true — so
// one frame owes exactly (cluster size − 1) receipts. The difference between this and what
// the listeners counted is the drop total, and it is the only way to obtain one, because
// the relay answers nothing on the wire and exports no counter.
func expectedDeliveries(o options, sentPerCluster []uint64) uint64 {
	var expected uint64
	for cluster, sent := range sentPerCluster {
		size := clusterSize(o.sessions, o.clusters, cluster)
		expected += sent * uint64(size-1)
	}
	return expected
}

// audibleSetSettle is how long the run waits past the last teleport before it starts
// counting, over and above the settle flag: the audible sets are recomputed only every
// game.VoiceSetInterval ticks.
func audibleSetSettle(tickRate int) time.Duration {
	return time.Duration(game.VoiceSetInterval) * time.Second / time.Duration(tickRate)
}

// makeKeyDir is where the run's throwaway signing pair lives.
//
// A directory rather than an in-memory key because internal/ticket mints one only through
// its key store, which is the same discipline the account service uses. It is removed at
// the end of the run: the pair admits sessions to one ephemeral world that no longer exists.
func makeKeyDir() (string, error) {
	dir, err := os.MkdirTemp("", "voxelheim-voicebot-")
	if err != nil {
		return "", fmt.Errorf("make a directory for the run's signing key: %w", err)
	}
	return dir, nil
}

func removeKeyDir(dir string) {
	if err := os.RemoveAll(dir); err != nil {
		fmt.Fprintf(os.Stderr, "voxelheim-voicebot: could not remove %s: %v\n", dir, err)
	}
}
