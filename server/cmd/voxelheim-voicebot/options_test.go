package main

import (
	"flag"
	"math"
	"strings"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
)

// goodOptions is a plan that passes every check, so each case below can change exactly one
// thing. A table of complete option sets would hide which field the refusal was about.
func goodOptions() options {
	var opts options
	registerPlanFlags(flag.NewFlagSet("test", flag.ContinueOnError), &opts)
	return opts
}

func TestThePlanFlagsDefaultToARunnableRun(t *testing.T) {
	t.Parallel()

	opts := goodOptions()
	if err := opts.validate(); err != nil {
		t.Fatalf("the defaults do not validate: %v", err)
	}
	if err := checkInsideTheWorld(opts); err != nil {
		t.Fatalf("the defaults fall off the world: %v", err)
	}
	if opts.sessions != session.MinConcurrentSessions {
		t.Errorf("the default session count is %d, want the server's own floor %d", opts.sessions, session.MinConcurrentSessions)
	}
	if float64(opts.frameRate) > game.VoiceRefillPerSecond {
		t.Errorf("the default frame rate %d is above the server's refill %v, so the default run measures the limiter",
			opts.frameRate, game.VoiceRefillPerSecond)
	}
}

// **Every refusal is checked for the sentence as well as the failure**, because a message
// that does not name the flag it is about is a message an operator reads as a bug in this
// command.
func TestTheOptionsRefuseWhatCannotBeMeasured(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name   string
		change func(*options)
		says   string
	}{
		{"no sessions", func(o *options) { o.sessions = 0 }, "sessions must be in"},
		{"past the server's ceiling", func(o *options) { o.sessions = session.MaxConcurrentSessions + 1 }, "sessions must be in"},
		{"more clusters than sessions", func(o *options) { o.clusters = o.sessions + 1 }, "clusters must be in"},
		{"no clusters", func(o *options) { o.clusters = 0 }, "clusters must be in"},
		{"a negative radius", func(o *options) { o.clusterRadius = -1 }, "must not be negative"},
		{"a voice range of zero", func(o *options) { o.voiceRange = 0 }, "finite positive number of blocks"},
		{"a voice range that is not a number", func(o *options) { o.voiceRange = math.NaN() }, "finite positive number of blocks"},
		{"a cluster wider than it can hear", func(o *options) { o.clusterRadius = 13 }, "a cluster has to be able to hear itself"},
		{"clusters that are one cluster", func(o *options) { o.clusterSpacing = 27 }, "they would be one cluster"},
		{"a fraction above one", func(o *options) { o.speaking = 1.5 }, "must be in 0..1"},
		{"a negative fraction", func(o *options) { o.speaking = -0.1 }, "must be in 0..1"},
		{"no window", func(o *options) { o.duration = 0 }, "positive duration"},
		{"a rate the limiter would decide", func(o *options) { o.frameRate = int(game.VoiceRefillPerSecond) + 1 }, "measures the limiter"},
		{"no frames at all", func(o *options) { o.frameRate = 0 }, "frame rate must be in"},
		{"a frame too small to time", func(o *options) { o.opusBytes = minSilenceBytes - 1 }, "an opus frame must be"},
		{"a frame past the contract", func(o *options) { o.opusBytes = maxSilenceBytes + 1 }, "an opus frame must be"},
		{"a world nobody can name", func(o *options) { o.worldName = "Not A World" }, "world name"},
	}
	for _, c := range cases {
		opts := goodOptions()
		c.change(&opts)
		err := opts.validate()
		if err == nil {
			t.Errorf("%s was accepted", c.name)
			continue
		}
		if !strings.Contains(err.Error(), c.says) {
			t.Errorf("%s was refused with %q, which does not say %q", c.name, err, c.says)
		}
	}
}

// A cluster exactly as wide as the range carries is accepted, and one block wider is not.
// The boundary is the whole of the rule, so it is the thing worth pinning.
func TestAClusterMayBeExactlyAsWideAsItsVoiceCarries(t *testing.T) {
	t.Parallel()

	opts := goodOptions()
	opts.clusterRadius = int(opts.voiceRange) / 2
	if err := opts.validate(); err != nil {
		t.Errorf("a cluster of diameter %v, exactly the voice range, was refused: %v", opts.voiceRange, err)
	}
	opts.clusterRadius++
	if err := opts.validate(); err == nil {
		t.Error("a cluster one block wider than its voice range was accepted")
	}
}

func TestSpeakersAreRoundedUpSoAFractionAboveZeroIsAlwaysAVoice(t *testing.T) {
	t.Parallel()

	cases := []struct {
		size     int
		fraction float64
		want     int
	}{
		{10, 0, 0},
		{10, 0.3, 3},
		{10, 0.01, 1},
		{1000, 0.1, 100},
		{7, 0.5, 4},
		{10, 1, 10},
	}
	for _, c := range cases {
		if got := speakersPerCluster(c.size, c.fraction); got != c.want {
			t.Errorf("speakersPerCluster(%d, %v) is %d, want %d", c.size, c.fraction, got, c.want)
		}
	}
}

func TestABareArgumentIsRefusedRatherThanIgnored(t *testing.T) {
	t.Parallel()

	_, err := parseFlags("test", []string{"sessions=100"})
	if err == nil || !strings.Contains(err.Error(), "unexpected argument") {
		t.Errorf("a bare argument was answered with %v; a mistyped flag would otherwise run the defaults and report them as asked for", err)
	}
}

// **The probe is a different command wearing the same binary**, so the flags it needs are
// checked on its own path: a probe that reached a TLS handshake before noticing it had no
// ticket would report the server's refusal as its own answer.

// A ticket comes out of the record a client caches, which is longer than a ticket.
