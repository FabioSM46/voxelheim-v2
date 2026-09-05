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
// The soak starts the server it measures, which is what gives the run a process to read
// /proc for and one command line in the ADR instead of a page of shell.
//
//	go run ./cmd/voxelheim-voicebot -h
//	go run ./cmd/voxelheim-voicebot -plan -sessions 100 -clusters 10 -speaking 0.3
//	go build -o /tmp/voxelheimd ./cmd/voxelheimd
//	go run ./cmd/voxelheim-voicebot -server /tmp/voxelheimd -sessions 100 -clusters 10 -speaking 0.3
package main

import (
	"context"
	"errors"
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

	if invocation.plan {
		if err := printPlan(os.Stdout, invocation.options); err != nil {
			fmt.Fprintf(os.Stderr, "voxelheim-voicebot: %v\n", err)
			os.Exit(1)
		}
		return
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	work := func() error { return runSoak(ctx, invocation.options, invocation.run, os.Stdout) }
	if invocation.probe {
		work = func() error { return runProbe(ctx, invocation.probeOptions, os.Stdout) }
	}
	if err := work(); err != nil {
		fmt.Fprintf(os.Stderr, "voxelheim-voicebot: %v\n", err)
		os.Exit(1)
	}
}

// parseFlags builds both option sets from one argument list and validates them together.
//
// The flag set is created here rather than taken from flag.CommandLine so a test can parse
// an argument list without touching process state — the shape cmd/voxelheimd's own flag
// helpers use, for the same reason.
func parseFlags(name string, args []string) (invocation, error) {
	flags := flag.NewFlagSet(name, flag.ContinueOnError)

	var call invocation
	registerPlanFlags(flags, &call.options)
	registerRunFlags(flags, &call.run)
	registerProbeFlags(flags, &call.probeOptions)
	flags.BoolVar(&call.plan, "plan", false,
		"print the layout the other flags describe and exit, without starting a server or "+
			"connecting anything. The geometry is a function of the flags and the seed alone")
	flags.BoolVar(&call.probe, "probe", false,
		"join one session on a server this command did not start and report the first voice "+
			"frame relayed to it. This is what scripts/interop-check.sh uses; it measures nothing")

	if err := flags.Parse(args); err != nil {
		return call, err
	}
	if flags.NArg() > 0 {
		return call, fmt.Errorf("unexpected argument %q; this command takes flags only", flags.Arg(0))
	}
	if call.plan && call.probe {
		return call, errors.New("invalid flags: -plan describes a soak and -probe is not one; ask for one of them")
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
	if err := call.run.validate(call.options, call.plan); err != nil {
		return call, fmt.Errorf("invalid flags: %w", err)
	}
	return call, nil
}

// invocation is one command line, resolved: which of the three things this command does,
// and the options that thing needs.
type invocation struct {
	options      options
	run          runOptions
	probeOptions probeOptions
	plan         bool
	probe        bool
}
