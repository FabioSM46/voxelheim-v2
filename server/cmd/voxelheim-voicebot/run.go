package main

import (
	"flag"
	"fmt"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
)

// The flags that describe the run rather than the plan: which server binary to start, how
// it is configured, and how the measured window is timed. They live beside the run they
// belong to, so main.go is the one command line and nothing else.

// runOptions are the flags that describe the run rather than the plan: where the server is,
// how it is configured, and how the window is timed.
type runOptions struct {
	serverBin      string
	maxPlayers     int
	tickRate       int
	viewDistance   int
	serverLogLevel string
	settle         time.Duration
	sampleEvery    time.Duration
	ticketKey      string
}

func registerRunFlags(flags *flag.FlagSet, run *runOptions) {
	flags.StringVar(&run.serverBin, "server", "",
		"the voxelheimd binary to run. Required unless -plan: this command starts the server it "+
			"measures, so that the process it reads /proc for is one it knows the configuration of")
	flags.IntVar(&run.maxPlayers, "max-players", 0,
		"the server's session ceiling. Zero means whichever is larger of -sessions and the "+
			"server's own floor, session.MinConcurrentSessions")
	flags.IntVar(&run.tickRate, "tick-rate", defaultTickRate,
		"the server's authoritative tick rate, in hertz. It is also the heartbeat every session sends at")
	flags.IntVar(&run.viewDistance, "view-distance", defaultViewDistance,
		"the server's chunk-streaming radius, in chunks. It is not a voice setting, and it is the "+
			"largest thing voice is competing with, so a run that changes it is not comparable with one that did not")
	flags.StringVar(&run.serverLogLevel, "server-log-level", "info",
		"the level the server logs at: `debug` or `info`, and nothing quieter, because the address "+
			"and the certificate this command dials are read out of the server's own Info startup "+
			"lines. Every voice refusal is a Debug line, so only debug can attribute a drop to the "+
			"limiter, the size cap or the audience — at the cost of one line per dropped delivery, "+
			"which at a thousand sessions is its own load")
	flags.DurationVar(&run.settle, "settle", 3*time.Second,
		"how long to wait after the last session is placed before the window opens. It has to "+
			"cover a recompute of the audible sets, which happens every game.VoiceSetInterval ticks")
	flags.DurationVar(&run.sampleEvery, "sample-every", time.Second,
		"how often the server's processor time and resident size are read from /proc")
}

// defaultViewDistance is the server's chunk-streaming radius a run asks for. It has a
// default here rather than deferring to the server's because a soak run has to state it: it
// is the largest thing voice competes with, and a report that did not name it could not be
// compared with another.
const defaultViewDistance = 3

func (r *runOptions) validate(o options, plan bool) error {
	if r.maxPlayers == 0 {
		// The server's own accepted range starts at session.MinConcurrentSessions, so a
		// small run still asks for that floor rather than being refused for asking for
		// exactly what it needs.
		r.maxPlayers = max(o.sessions, session.MinConcurrentSessions)
	}
	switch {
	case !plan && r.serverBin == "":
		return fmt.Errorf("-server is required: this command starts the voxelheimd it measures (build one with `go build -o <path> ./cmd/voxelheimd`)")
	case r.maxPlayers < o.sessions:
		return fmt.Errorf("a ceiling of %d players cannot admit %d sessions", r.maxPlayers, o.sessions)
	case r.tickRate < 1 || r.tickRate > 255:
		return fmt.Errorf("the tick rate must be in 1..255, got %d", r.tickRate)
	case r.viewDistance < 0:
		return fmt.Errorf("the view distance must not be negative, got %d", r.viewDistance)
	case r.settle < audibleSetSettle(r.tickRate):
		return fmt.Errorf(
			"a settle of %v is shorter than one recompute of the audible sets at %d Hz (%v); frames sent before it "+
				"would be counted as owed and never delivered", r.settle, r.tickRate, audibleSetSettle(r.tickRate))
	case r.sampleEvery <= 0:
		return fmt.Errorf("the sampling interval must be positive, got %v", r.sampleEvery)
	}
	switch r.serverLogLevel {
	case "debug", "info":
	default:
		// warn and error are levels voxelheimd accepts and this command cannot use: it reads
		// the listening address and the certificate fingerprint out of two Info lines, so a
		// quieter server is one it can start and never find. Refused here rather than
		// discovered as a run that hangs.
		return fmt.Errorf(
			"the server log level must be debug or info, got %q: this command reads the address and the "+
				"certificate fingerprint out of the server's own startup lines, and both are Info",
			r.serverLogLevel)
	}
	return nil
}
