// Command voxelheim-voicebot fills a Voxelheim server with synthetic speakers and reports
// what proximity voice costs it.
//
// **It is a measurement harness and it is not a test.** Nothing here asserts a number. It
// connects the sessions a plan describes, has a configurable share of them speak at a real
// cadence with a real frame size, and prints what came back. The numbers it produces are
// recorded in docs/adr/0001-voice-transport.md under "Measured", with the command that
// produced them.
//
// **It is deliberately not a CI step.** It wants a whole machine for half a minute and its
// answers are about that machine; a gate that turned a slow runner into a red pull request
// would be measuring the runner. Run it by hand, on a machine you are prepared to describe.
//
// What it does today is describe the run — the geometry, the fan-out and the frame — and
// exit. That is the first of #855's parts: a plan is checkable before a thousand sessions
// have been paid for, and a mistake in the geometry is invisible in the numbers afterwards.
//
//	go run ./cmd/voxelheim-voicebot -h
//	go run ./cmd/voxelheim-voicebot -sessions 100 -clusters 10 -speaking 0.3
//	go run ./cmd/voxelheim-voicebot -sessions 1000 -clusters 1 -cluster-radius 10 -speaking 0.1
package main

import (
	"flag"
	"fmt"
	"os"
)

func main() {
	opts, err := parseFlags(os.Args[0], os.Args[1:])
	if err != nil {
		fmt.Fprintf(os.Stderr, "voxelheim-voicebot: %v\n", err)
		os.Exit(2)
	}
	if err := printPlan(os.Stdout, opts); err != nil {
		fmt.Fprintf(os.Stderr, "voxelheim-voicebot: %v\n", err)
		os.Exit(1)
	}
}

// parseFlags builds the option set from one argument list and validates it.
//
// The flag set is created here rather than taken from flag.CommandLine so a test can parse
// an argument list without touching process state — the shape cmd/voxelheimd's own flag
// helpers use, for the same reason.
func parseFlags(name string, args []string) (options, error) {
	flags := flag.NewFlagSet(name, flag.ContinueOnError)

	var opts options
	registerPlanFlags(flags, &opts)

	if err := flags.Parse(args); err != nil {
		return opts, err
	}
	if flags.NArg() > 0 {
		return opts, fmt.Errorf("unexpected argument %q; this command takes flags only", flags.Arg(0))
	}
	if err := opts.validate(); err != nil {
		return opts, fmt.Errorf("invalid flags: %w", err)
	}
	if err := checkInsideTheWorld(opts); err != nil {
		return opts, fmt.Errorf("invalid flags: %w", err)
	}
	return opts, nil
}
