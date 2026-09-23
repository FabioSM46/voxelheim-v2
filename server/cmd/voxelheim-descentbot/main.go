// Command voxelheim-descentbot plays the first dungeon end to end against a real voxelheimd
// with a party of bots and reports the run: each part's wall clock, each member's deaths, the
// creatures met and killed, the development commands used and the estimated human time
// (#1298, played as a party since #1333). It is an acceptance run, not a CI step: it wants a
// machine for half an hour and its clock is that machine's.
//
//	go build -o <output-directory>/voxelheimd ./cmd/voxelheimd
//	go run ./cmd/voxelheim-descentbot -server <output-directory>/voxelheimd -party 3
package main

import (
	"context"
	"crypto/sha256"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"regexp"
	"syscall"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

type options struct {
	serverBin    string
	seed         int64
	worldName    string
	viewDistance int
	timeout      time.Duration
	maxWipes     int
	members      int
	quiet        bool
}

const tickRate = 20

func parseFlags(name string, args []string) (options, error) {
	flags := flag.NewFlagSet(name, flag.ContinueOnError)
	var o options
	flags.StringVar(&o.serverBin, "server", "", "the voxelheimd binary to start and play against (required)")
	flags.Int64Var(&o.seed, "seed", 1, "the open world's seed; its portal ruin is where the run begins")
	flags.StringVar(&o.worldName, "world-name", "descent", "the world the bot's ticket is minted for")
	flags.IntVar(&o.viewDistance, "view-distance", 4,
		"the server's streaming radius in chunks; the bot plans only over terrain it was sent")
	flags.DurationVar(&o.timeout, "timeout", 60*time.Minute, "give up on the run after this long")
	flags.IntVar(&o.maxWipes, "max-wipes", 3,
		"wipes in a row, every member dead at once with nothing killed between, before the party gives up the run")
	flags.IntVar(&o.members, "party", 3,
		fmt.Sprintf("how many bots play, each its own character in one party: 1 to %d", len(memberNames)))
	flags.BoolVar(&o.quiet, "quiet", false, "print only the report, not the run's progress")
	if err := flags.Parse(args); err != nil {
		return o, err
	}
	switch {
	case flags.NArg() > 0:
		return o, fmt.Errorf("unexpected argument %q; this command takes flags only", flags.Arg(0))
	case o.serverBin == "":
		return o, errors.New("-server is required: build one with `go build -o <path> ./cmd/voxelheimd`")
	case o.viewDistance < 2 || o.viewDistance > 16:
		return o, fmt.Errorf("-view-distance must be in 2..16, got %d", o.viewDistance)
	case o.timeout <= 0:
		return o, fmt.Errorf("-timeout must be positive, got %v", o.timeout)
	case o.members < 1 || o.members > len(memberNames):
		return o, fmt.Errorf("-party must be in 1..%d, got %d", len(memberNames), o.members)
	case o.maxWipes < 1:
		return o, fmt.Errorf("-max-wipes must be at least 1, got %d", o.maxWipes)
	case !regexp.MustCompile(`^[a-z0-9-]{1,32}$`).MatchString(o.worldName):
		return o, fmt.Errorf("-world-name %q is not lowercase letters, digits and hyphens", o.worldName)
	}
	return o, nil
}

func main() {
	o, err := parseFlags(os.Args[0], os.Args[1:])
	if err != nil {
		fmt.Fprintf(os.Stderr, "voxelheim-descentbot: %v\n", err)
		os.Exit(2)
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	ctx, cancel := context.WithTimeout(ctx, o.timeout)
	defer cancel()
	if err := run(ctx, o, os.Stdout, os.Stderr); err != nil {
		fmt.Fprintf(os.Stderr, "voxelheim-descentbot: %v\n", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, o options, out, progress io.Writer) error {
	maxWipes = o.maxWipes
	keyDir, err := os.MkdirTemp("", "voxelheim-descentbot-")
	if err != nil {
		return fmt.Errorf("make a directory for the signing key: %w", err)
	}
	defer func() { _ = os.RemoveAll(keyDir) }()
	pair, err := ticket.LoadOrCreate(keyDir)
	if err != nil {
		return fmt.Errorf("mint a signing key: %w", err)
	}
	worldID, err := ticket.WorldIDFor(o.worldName)
	if err != nil {
		return err
	}
	server, err := startServer(ctx, o, pair.PublicHex())
	if err != nil {
		return err
	}
	defer server.stop()

	tally := newTally()
	say := func(name string) func(string, ...any) {
		return func(format string, args ...any) {
			if !o.quiet {
				_, _ = fmt.Fprintf(progress, "%s  %-7s %s\n", time.Now().Format("15:04:05"), name, fmt.Sprintf(format, args...))
			}
		}
	}
	session, end := context.WithCancel(ctx)
	defer end()
	readErrs := make(chan error, o.members)
	members := make([]*runner, 0, o.members)
	for _, name := range memberNames[:o.members] {
		sum := sha256.Sum256([]byte(name))
		var account ticket.AccountID
		copy(account[:], sum[:])
		credential, _, err := pair.Mint(account, worldID, time.Now())
		if err != nil {
			return fmt.Errorf("mint a ticket: %w", err)
		}
		stats := newRunStats(tally)
		c, err := join(ctx, server.addr, server.fingerprint, name, credential[:], stats)
		if err != nil {
			return fmt.Errorf("join %s: %w", name, err)
		}
		defer func() { _ = c.conn.Close() }()
		go func() { readErrs <- c.listen(session) }()
		go c.drive(session, tickRate)
		members = append(members, &runner{
			pilot: &pilot{c: c, stats: stats, rate: tickRate, say: say(name), unreachable: map[uint64]bool{}},
			opts:  o, server: server, timing: map[string]time.Duration{}, rss: map[string]uint64{}, caveBase: -1,
		})
	}
	pt := newParty(members)
	if err := sleep(ctx, 2*time.Second); err != nil {
		return err
	}
	playErr := pt.play(session)
	end()
	for _, m := range members {
		m.stats.finish()
	}
	writeReport(out, pt, playErr)
	if playErr != nil {
		return playErr
	}
	select {
	case err := <-readErrs:
		return err
	default:
		return nil
	}
}
