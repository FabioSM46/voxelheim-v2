package main

import (
	"flag"
	"fmt"
	"math"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The flag set, and the rule that every value is checked before anything is derived from
// it — the pattern cmd/voxelheim-auth states and cmd/voxelheimd copies. A soak test that
// silently narrowed `-speaking 3` to "everybody" would publish a number nobody asked for
// under a heading that says it was measured.
//
// Every flag is registered through a helper taking a *flag.FlagSet, for the reason
// voxelheimd's -voice-range is: a test builds a fresh set rather than reaching into
// flag.CommandLine.

// Defaults chosen so the two runs docs/adr/0001-voice-transport.md records are one flag
// away from each other rather than a page of them.
const (
	// defaultFrameRate is 50 frames a second: the 20 ms frame the client encodes, and the
	// cadence internal/transport's relay measurement already used, so the two reports are
	// comparable. Deliberately under game.VoiceRefillPerSecond, which is what makes the
	// limiter a check on this command rather than a term in its results.
	defaultFrameRate = 50

	// defaultOpusBytes is the 96-byte payload internal/transport's measurement used, which
	// is a 20 ms frame at about 38 kbit/s — a real voice bitrate, and well inside the
	// contract's 400-byte ceiling.
	defaultOpusBytes = 96

	// defaultClusterRadius is a huddle rather than a crowd: everybody in a cluster is
	// within the default voice range of everybody else, which is what makes the audible
	// set the whole cluster and the fan-out predictable.
	defaultClusterRadius = 8

	// defaultClusterSpacing puts cluster centres far enough apart that no member of one
	// can hear any member of another, with a wide margin over the hysteresis limit.
	defaultClusterSpacing = 512

	// maxClusterRadius bounds the disc [latticeDisc] materialises.
	//
	// **The lattice is a slice, so a radius costs its square.** Nothing else here bounds it:
	// the geometry rule is that a cluster must fit inside its own voice range, and
	// -voice-range is a finite positive number of blocks with no ceiling below the float32
	// the welcome announces. A radius of a million would ask for three trillion positions
	// before any check could refuse the layout they fall outside of.
	//
	// 256 is generous by two orders of magnitude in the direction that matters: its disc
	// holds about 205,000 blocks and a run places at most session.MaxConcurrentSessions
	// sessions on them, and reaching it at all needs a voice that carries 512 blocks.
	maxClusterRadius = 256

	defaultDuration  = 30 * time.Second
	defaultWorldName = "soak"
)

// The bounds an Opus frame this command sends has to sit inside.
//
// **The ceiling is the contract's and the floor is this command's.** A frame past
// [protocol.MaxVoiceOpusBytes] is one the relay drops by design, so a run built out of them
// would measure the size cap and nothing else. The floor is narrower than Opus allows: the
// last eight bytes of every frame carry the instant it was sent, which is what the relay
// latency is measured from, and a frame with no room for them is one this command could
// count but never time.
const (
	// opusStampBytes is the width of that instant: a nanosecond count, big-endian, in the
	// last bytes of the frame. It lives beside the bound it produces rather than beside the
	// builder that writes it, because it is the option set that has to refuse a size too
	// small to hold one.
	opusStampBytes = 8

	minSilenceBytes = 2 + 1 + opusStampBytes
	maxSilenceBytes = protocol.MaxVoiceOpusBytes
)

type options struct {
	sessions       int
	clusters       int
	clusterRadius  int
	clusterSpacing int
	speaking       float64
	duration       time.Duration
	frameRate      int
	opusBytes      int
	voiceRange     float64
	seed           int64
	worldName      string
}

func registerPlanFlags(flags *flag.FlagSet, opts *options) {
	flags.IntVar(&opts.sessions, "sessions", session.MinConcurrentSessions,
		"how many synthetic sessions to connect. The server's own ceiling is "+
			"session.MaxConcurrentSessions and -max-players has to be at least this")
	flags.IntVar(&opts.clusters, "clusters", 1,
		"how many separate huddles to spread those sessions over. Cluster sizes differ by at "+
			"most one when the count does not divide")
	flags.IntVar(&opts.clusterRadius, "cluster-radius", defaultClusterRadius,
		"the radius of one huddle, in blocks. Members are placed on the integer lattice inside "+
			"it, so twice this must stay inside -voice-range or a cluster cannot hear itself")
	flags.IntVar(&opts.clusterSpacing, "cluster-spacing", defaultClusterSpacing,
		"how far apart cluster centres are, in blocks. It has to exceed the range voice actually "+
			"carries — game.VoiceExitFactor widens the audible set past -voice-range — or two "+
			"clusters are one")
	flags.Float64Var(&opts.speaking, "speaking", 0.3,
		"the fraction of sessions that speak, in 0..1. The speakers are the first members of "+
			"each cluster, so every cluster carries the same share rather than one carrying all of it")
	flags.DurationVar(&opts.duration, "duration", defaultDuration,
		"how long the measured window lasts, after every session has joined and settled")
	flags.IntVar(&opts.frameRate, "frame-rate", defaultFrameRate,
		"frames a second each speaker sends. Above game.VoiceRefillPerSecond the server's limiter "+
			"is what the run measures, which is not what this command is for")
	flags.IntVar(&opts.opusBytes, "opus-bytes", defaultOpusBytes,
		"the size of one synthetic Opus silence frame, in bytes. The last eight carry the send "+
			"instant the relay latency is measured from")
	flags.Float64Var(&opts.voiceRange, "voice-range", game.VoiceRangeDefault,
		"how far a voice carries, in blocks. Passed to the server and used here to check that a "+
			"cluster is inside its own range")
	flags.Int64Var(&opts.seed, "seed", 1,
		"the world seed. It decides the spawn the clusters are laid out from, so the same seed is "+
			"the same geometry")
	flags.StringVar(&opts.worldName, "world-name", defaultWorldName,
		"the world these sessions present tickets for. Lowercase letters, digits and hyphens")
}

// validate checks the raw flags, quoting what was typed.
//
// **Nothing is clamped.** `-speaking 3` is a mistake about what the fraction means, and a
// run that answered it with "everybody" would put a number in an ADR under a heading that
// claims it was asked for. The order below is the order a reader would ask the questions
// in: how many bots, where they stand, and what they say.
func (o options) validate() error {
	switch {
	case o.sessions < 1 || o.sessions > session.MaxConcurrentSessions:
		return fmt.Errorf("sessions must be in 1..%d, got %d", session.MaxConcurrentSessions, o.sessions)
	case o.clusters < 1 || o.clusters > o.sessions:
		return fmt.Errorf("clusters must be in 1..%d, one per session at most, got %d", o.sessions, o.clusters)
	case o.clusterRadius < 0:
		return fmt.Errorf("cluster radius must not be negative, got %d", o.clusterRadius)
	case o.clusterRadius > maxClusterRadius:
		// Refused before the geometry is checked, because the geometry is checked by
		// laying the plan out and a radius past this one is a lattice nothing can hold.
		return fmt.Errorf(
			"cluster radius must be at most %d, got %d: the lattice inside a cluster is materialised, so a "+
				"radius costs its square, and %d already offers %d blocks for at most %d sessions to stand on",
			maxClusterRadius, o.clusterRadius, maxClusterRadius,
			len(latticeDisc(maxClusterRadius)), session.MaxConcurrentSessions)
	case math.IsNaN(o.voiceRange) || math.IsInf(o.voiceRange, 0) || o.voiceRange <= 0:
		// Zero is a legal -voice-range for a server and means it relays nothing at all,
		// which is a server this command has nothing to measure on.
		return fmt.Errorf("voice range must be a finite positive number of blocks, got %v", o.voiceRange)
	case float64(2*o.clusterRadius) > o.voiceRange:
		// Two members on opposite edges are 2r apart. Past the range they are not in each
		// other's audible set, the expected fan-out this command reports would be wrong,
		// and every frame missing from the difference would be read as a drop.
		return fmt.Errorf(
			"a cluster radius of %d blocks puts members up to %d apart, which a voice range of %v does not carry; "+
				"a cluster has to be able to hear itself", o.clusterRadius, 2*o.clusterRadius, o.voiceRange)
	case float64(o.clusterSpacing) <= o.voiceRange*game.VoiceExitFactor+float64(2*o.clusterRadius):
		// A negative spacing fails this comparison too, which is why there is no separate
		// sign check: clusters that marched backwards would be one cluster before they were
		// anything else, and [checkInsideTheWorld] leans on their not doing so.
		// The audible set is hysteretic: a listener leaves it only at the range widened by
		// VoiceExitFactor, so that widened number is the one two clusters have to clear —
		// plus the two radii, because the nearest members are not the centres.
		return fmt.Errorf(
			"a cluster spacing of %d blocks does not separate two clusters of radius %d at a voice range of %v "+
				"(hearing reaches %v blocks and leaves only at %v); they would be one cluster",
			o.clusterSpacing, o.clusterRadius, o.voiceRange,
			o.voiceRange, o.voiceRange*game.VoiceExitFactor)
	case math.IsNaN(o.speaking) || o.speaking < 0 || o.speaking > 1:
		return fmt.Errorf("the speaking fraction must be in 0..1, got %v", o.speaking)
	case o.duration <= 0:
		return fmt.Errorf("the measured window must be a positive duration, got %v", o.duration)
	case o.frameRate < 1 || float64(o.frameRate) > game.VoiceRefillPerSecond:
		return fmt.Errorf(
			"the frame rate must be in 1..%v — the server refills a speaker's allowance at %v frames a second and "+
				"anything above it measures the limiter — got %d",
			game.VoiceRefillPerSecond, game.VoiceRefillPerSecond, o.frameRate)
	case o.opusBytes < minSilenceBytes || o.opusBytes > maxSilenceBytes:
		return fmt.Errorf("an opus frame must be %d..%d bytes, got %d", minSilenceBytes, maxSilenceBytes, o.opusBytes)
	}
	if _, err := ticket.WorldIDFor(o.worldName); err != nil {
		return fmt.Errorf("world name %q: %w", o.worldName, err)
	}
	return nil
}

// origin is where the clusters are laid out from: the spawn this seed produces.
//
// Read from internal/world rather than from a running server, so a plan can be printed
// and checked before anything is connected. It is the same function cmd/voxelheimd calls
// to fill ServerWelcome.spawn, so the two cannot disagree.
func (o options) origin() [3]int {
	spawn := world.SpawnAt(o.seed)
	return [3]int{int(spawn[0]), int(spawn[1]), int(spawn[2])}
}

// speakersPerCluster is how many of a cluster's members speak.
//
// Rounded up rather than down, so `-speaking` above zero always produces a speaker in
// every cluster: a fraction that silently produced none would be a run measuring nothing,
// reported as a run that measured no drops.
func speakersPerCluster(size int, fraction float64) int {
	speakers := int(math.Ceil(float64(size) * fraction))
	if speakers > size {
		return size
	}
	return speakers
}
