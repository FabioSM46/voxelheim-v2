// Command voxelheim-voicebot fills a Voxelheim server with synthetic speakers and reports
// what proximity voice costs it.
//
// **It is a measurement harness and it is not a test.** Nothing here asserts a number. It
// connects the sessions a plan describes, has a configurable share of them speak at a real
// cadence with a real frame size, and prints what came back: how many frames were owed, how
// many arrived, how long each took, how fast the simulation actually ticked, and what the
// server's processor and memory cost was while it happened. The numbers it produces are
// recorded in docs/adr/0001-voice-transport.md under "Measured", with the command that
// produced them.
//
// **It is deliberately not a CI step.** It wants a whole machine for half a minute and its
// answers are about that machine; a gate that turned a slow runner into a red pull request
// would be measuring the runner. Run it by hand, on a machine you are prepared to describe.
//
// Two of the three things it does are here: it describes the run — the geometry, the
// fan-out and the frame — and it joins one session on a server somebody else started to say
// whether a voice frame reached it. The second is what scripts/interop-check.sh needs from
// the Go half of #855; the soak that starts a server of its own is the part after this one.
//
//	go run ./cmd/voxelheim-voicebot -h
//	go run ./cmd/voxelheim-voicebot -sessions 100 -clusters 10 -speaking 0.3
//	go run ./cmd/voxelheim-voicebot -probe -addr 127.0.0.1:7777 -fingerprint <64 hex> -ticket-file <path>
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"syscall"
)

func main() {
	invocation, err := parseFlags(os.Args[0], os.Args[1:])
	if err != nil {
		fmt.Fprintf(os.Stderr, "voxelheim-voicebot: %v\n", err)
		os.Exit(2)
	}

	if !invocation.probe {
		if err := printPlan(os.Stdout, invocation.options); err != nil {
			fmt.Fprintf(os.Stderr, "voxelheim-voicebot: %v\n", err)
			os.Exit(1)
		}
		return
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	if err := runProbe(ctx, invocation.probeOptions, os.Stdout); err != nil {
		fmt.Fprintf(os.Stderr, "voxelheim-voicebot: %v\n", err)
		os.Exit(1)
	}
}

// parseFlags builds both option sets from one argument list and validates the one the
// invocation is actually about.
//
// The flag set is created here rather than taken from flag.CommandLine so a test can parse
// an argument list without touching process state — the shape cmd/voxelheimd's own flag
// helpers use, for the same reason.
func parseFlags(name string, args []string) (invocation, error) {
	flags := flag.NewFlagSet(name, flag.ContinueOnError)

	var call invocation
	registerPlanFlags(flags, &call.options)
	registerProbeFlags(flags, &call.probeOptions)
	flags.BoolVar(&call.probe, "probe", false,
		"join one session on a server this command did not start and report the first voice "+
			"frame relayed to it. This is what scripts/interop-check.sh uses; it measures nothing")

	if err := flags.Parse(args); err != nil {
		return call, err
	}
	if flags.NArg() > 0 {
		return call, fmt.Errorf("unexpected argument %q; this command takes flags only", flags.Arg(0))
	}
	if call.probe {
		if err := call.probeOptions.validate(); err != nil {
			return call, fmt.Errorf("invalid flags: %w", err)
		}
		return call, nil
	}
	if err := call.options.validate(); err != nil {
		return call, fmt.Errorf("invalid flags: %w", err)
	}
	if err := checkInsideTheWorld(call.options); err != nil {
		return call, fmt.Errorf("invalid flags: %w", err)
	}
	return call, nil
}

// invocation is one command line, resolved: which of the things this command does, and the
// options that thing needs.
type invocation struct {
	options      options
	probeOptions probeOptions
	probe        bool
}
