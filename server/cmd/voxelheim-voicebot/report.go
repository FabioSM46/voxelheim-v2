package main

import (
	"context"
	"fmt"
	"io"
	"time"
)

// What a run says, and the care taken over what it does not.
//
// Three numbers are measured directly — frames sent, frames heard, and how long each receipt
// took — and everything else is derived from them or read off the operating system. The
// derivations are stated in the report itself and not only here, because a number pasted
// into an ADR outlives the command that produced it.

// tickRateTolerance is how far under the configured rate the achieved rate may sit before
// the report calls the tick budget exceeded: one percent, which at 20 Hz is 0.5 ms of a
// 50 ms tick.
const tickRateTolerance = 0.01

// usageSummary is what one process cost over the measured window.
type usageSummary struct {
	samples    int
	cpuSeconds float64
	span       time.Duration
	peakRSS    uint64
	lastRSS    uint64
	err        error
}

// cores is the mean number of processors the process kept busy over the window.
func (u usageSummary) cores() float64 {
	if u.span <= 0 {
		return 0
	}
	return u.cpuSeconds / u.span.Seconds()
}

// sampler reads one process's cost at a fixed cadence for as long as the run lasts.
type sampler struct {
	done    chan struct{}
	stopped chan usageSummary
}

func startSampler(ctx context.Context, pid int, every time.Duration) *sampler {
	s := &sampler{done: make(chan struct{}), stopped: make(chan usageSummary, 1)}
	go func() {
		var summary usageSummary
		first, err := readUsage(pid)
		if err != nil {
			summary.err = err
			s.stopped <- summary
			return
		}
		startedAt := time.Now()
		lastAt := startedAt
		last := first
		summary.samples, summary.peakRSS = 1, first.rssBytes

		ticker := time.NewTicker(every)
		defer ticker.Stop()
		for {
			select {
			case <-s.done:
			case <-ctx.Done():
			case <-ticker.C:
				current, err := readUsage(pid)
				if err != nil {
					// A process that has gone is not a reading error worth losing the run
					// over; the samples already taken are the answer.
					summary.err = err
					continue
				}
				last, lastAt = current, time.Now()
				summary.samples++
				if current.rssBytes > summary.peakRSS {
					summary.peakRSS = current.rssBytes
				}
				continue
			}
			// The span is the distance between the two readings the delta was taken
			// from, not the length of the run. A run that ends between two ticks would
			// otherwise divide a five-tick delta by six ticks of wall clock and report a
			// processor cost lower than the one it measured.
			summary.cpuSeconds = last.cpuSeconds - first.cpuSeconds
			summary.span = lastAt.Sub(startedAt)
			summary.lastRSS = last.rssBytes
			s.stopped <- summary
			return
		}
	}()
	return s
}

func (s *sampler) stop() usageSummary {
	close(s.done)
	return <-s.stopped
}

// soakReport is one run's whole answer.
type soakReport struct {
	options     options
	run         runOptions
	commandLine string
	sessions    int
	window      time.Duration

	sent           uint64
	limiterRefused uint64
	heard          uint64
	unstamped      uint64
	expected       uint64
	latency        histogram

	speakers int

	firstTick, lastTick     uint32
	firstTickAt, lastTickAt time.Time
	haveTicks               bool

	server    usageSummary
	bot       usageSummary
	userCPU   time.Duration
	systemCPU time.Duration

	limiterLostDeliveries uint64

	limiterDrops  uint64
	sizeCapDrops  uint64
	audienceDrops uint64
	laneDrops     uint64
	fellBehind    uint64
	logLines      uint64

	readErrors []string
}

// absorb folds every session's counters into the report.
func (r *soakReport) absorb(bots []*bot) {
	sentPerCluster := make([]uint64, r.options.clusters)
	refusedPerCluster := make([]uint64, r.options.clusters)
	for _, b := range bots {
		sent := b.sent.Load()
		r.sent += sent
		r.limiterRefused += b.limiterRefused.Load()
		r.heard += b.heard.Load()
		r.unstamped += b.unstamped.Load()
		sentPerCluster[b.place.cluster] += sent
		refusedPerCluster[b.place.cluster] += b.limiterRefused.Load()
		if b.place.speaker {
			r.speakers++
		}
		r.latency.merge(&b.latency)

		if !b.haveTick {
			continue
		}
		if !r.haveTicks || b.firstTickAt.Before(r.firstTickAt) {
			r.firstTick, r.firstTickAt = b.firstTick, b.firstTickAt
		}
		if !r.haveTicks || b.lastTickAt.After(r.lastTickAt) {
			r.lastTick, r.lastTickAt = b.lastTick, b.lastTickAt
		}
		r.haveTicks = true
	}
	r.expected = expectedDeliveries(r.options, sentPerCluster)
	// A frame the limiter refuses reaches nobody, so it costs the whole cluster's worth of
	// deliveries. Subtracting them is what leaves the latency lane as the residual.
	r.limiterLostDeliveries = expectedDeliveries(r.options, refusedPerCluster)
}

func (r *soakReport) absorbServerLog(s *serverProcess) {
	r.limiterDrops = s.limiterDrops.Load()
	r.sizeCapDrops = s.sizeCapDrops.Load()
	r.audienceDrops = s.audienceDrops.Load()
	r.laneDrops = s.laneDrops.Load()
	r.fellBehind = s.fellBehind.Load()
	r.logLines = s.logLines.Load()
}

// achievedTickRate is how fast the simulation actually ticked over the measured window.
//
// **The only statement about the tick budget a bot outside the server can make, and one
// about the mean rather than any single tick.** EntitySnapshot carries server_tick, which
// the loop advances by exactly one per Sim.Step and never skips, so ticks per wall-clock
// second is the rate it achieved — and a rate under the configured one means the average
// Sim.Step plus the loop's overhead did not fit in the interval. A p99 would need an
// instrument inside the process, and this command deliberately does not put one there.
func (r *soakReport) achievedTickRate() (float64, bool) {
	if !r.haveTicks || r.lastTick <= r.firstTick {
		return 0, false
	}
	span := r.lastTickAt.Sub(r.firstTickAt)
	if span <= 0 {
		return 0, false
	}
	return float64(r.lastTick-r.firstTick) / span.Seconds(), true
}

func (r *soakReport) render(out io.Writer) error {
	l := &lines{to: out}
	o := r.options
	l.printf("voice soak — %d sessions, %d clusters, %.0f%% speaking\n\n", r.sessions, o.clusters, o.speaking*100)
	l.printf("  command      %s\n", r.commandLine)
	l.printf("  window       %s (measured), settle %s\n", r.window.Round(time.Millisecond), r.run.settle)
	l.printf("  speakers     %d of %d, %d frames/s each, %d-byte opus, voice range %v blocks\n",
		r.speakers, r.sessions, o.frameRate, o.opusBytes, o.voiceRange)
	l.printf("  geometry     clusters of %d..%d at radius %d, spaced %d blocks\n",
		clusterSize(o.sessions, o.clusters, o.clusters-1), clusterSize(o.sessions, o.clusters, 0),
		o.clusterRadius, o.clusterSpacing)

	l.printf("\nframes\n")
	l.printf("  sent          %d\n", r.sent)
	l.printf("  owed          %d  (one per listener in the speaker's own cluster)\n", r.expected)
	l.printf("  delivered     %d  (%s)\n", r.heard, percent(r.heard, r.expected))
	l.printf("  dropped       %d  (%s)\n", saturatingSub(r.expected, r.heard), percent(saturatingSub(r.expected, r.heard), r.expected))
	l.printf("  unstamped     %d  (received, but too short to time)\n", r.unstamped)

	l.printf("\ndrops by cause\n")
	l.printf("  limiter, predicted from this command's own send instants and game's two constants\n")
	l.printf("    frames refused    %d of %d sent (%s)\n", r.limiterRefused, r.sent, percent(r.limiterRefused, r.sent))
	l.printf("    deliveries lost   %d\n", r.limiterLostDeliveries)
	l.printf("  size cap            0 — every frame this command sends is %d bytes, of %d allowed\n",
		o.opusBytes, maxSilenceBytes)
	l.printf("  audience            0 — every frame asks for Everyone, which the relay recognises\n")
	l.printf("  latency lane        %d, the residual: owed − delivered − lost to the limiter\n",
		saturatingSub(saturatingSub(r.expected, r.heard), r.limiterLostDeliveries))

	l.printf("\nthe same question asked of the server's own diagnostics\n")
	if r.run.serverLogLevel != "debug" {
		l.printf("  the server ran at -log-level %s and every refusal below is a Debug line, so\n", r.run.serverLogLevel)
		l.printf("  these counts are zero because nothing was logged, not because nothing was\n")
		l.printf("  dropped. Re-run with -server-log-level debug to attribute them.\n")
	}
	l.printf("  limiter       %d\n", r.limiterDrops)
	l.printf("  size cap      %d\n", r.sizeCapDrops)
	l.printf("  audience      %d\n", r.audienceDrops)
	l.printf("  latency lane  %d\n", r.laneDrops)
	l.printf("  server log lines seen: %d\n", r.logLines)

	l.printf("\nrelay latency, send to receipt at a listener in the same cluster\n")
	l.printf("  measured inside this process, write to receipt, so it carries this command's\n")
	l.printf("  own receive scheduling as well as the server's relay — the bot's core figure\n")
	l.printf("  below is what says how much of it to believe.\n")
	l.printf("  samples       %d\n", r.latency.count())
	l.printf("  p50           %s\n", quantileText(&r.latency, 0.50))
	l.printf("  p99           %s\n", quantileText(&r.latency, 0.99))
	l.printf("  longest       %s\n", r.latency.max.Round(time.Microsecond))

	l.printf("\nthe tick loop\n")
	if rate, ok := r.achievedTickRate(); ok {
		budget := time.Second / time.Duration(r.run.tickRate)
		achieved := time.Duration(float64(time.Second) / rate)
		l.printf("  configured    %d Hz (%s per tick)\n", r.run.tickRate, budget)
		l.printf("  achieved      %.2f Hz (%s per tick, mean over %d ticks)\n",
			rate, achieved.Round(100*time.Microsecond), r.lastTick-r.firstTick)
		// **A band rather than a comparison.** The achieved rate is read from snapshot
		// arrival times on a thousand goroutines, so it carries scheduling noise of its
		// own; calling a tick over budget because it measured 20.0016 Hz would put a
		// finding in an ADR that belongs to the measurement.
		if rate < float64(r.run.tickRate)*(1-tickRateTolerance) {
			l.printf("  OVER BUDGET   the mean tick took %s, %s past its %s budget\n",
				achieved.Round(100*time.Microsecond), (achieved - budget).Round(100*time.Microsecond), budget)
		}
	} else {
		l.printf("  no snapshots carried two different ticks; nothing can be said\n")
	}
	l.printf("  \"fell behind\" warnings: %d\n", r.fellBehind)

	l.printf("\ncost\n")
	renderUsage(l, "  server", r.server)
	l.printf("    whole-life CPU %s user + %s system (the kernel's own accounting, joins included)\n",
		r.userCPU.Round(time.Millisecond), r.systemCPU.Round(time.Millisecond))
	renderUsage(l, "  bot   ", r.bot)

	if len(r.readErrors) > 0 {
		l.printf("\nsessions that ended badly: %d\n", len(r.readErrors))
		for _, err := range r.readErrors[:min(len(r.readErrors), 5)] {
			l.printf("  %s\n", err)
		}
	}
	return l.err
}

func renderUsage(l *lines, label string, u usageSummary) {
	if u.err != nil && u.samples == 0 {
		l.printf("%s  unavailable: %v\n", label, u.err)
		return
	}
	l.printf("%s  %.2f cores mean over %s, RSS %s at the end, %s peak (%d samples)\n",
		label, u.cores(), u.span.Round(time.Second), mib(u.lastRSS), mib(u.peakRSS), u.samples)
}

func quantileText(h *histogram, fraction float64) string {
	if h.count() == 0 {
		return "no samples"
	}
	value, exact := h.quantile(fraction)
	if !exact {
		return fmt.Sprintf("at least %s (the histogram's ceiling; longest was %s)", value, h.max.Round(time.Millisecond))
	}
	return value.String()
}

func percent(part, whole uint64) string {
	if whole == 0 {
		return "no frames were owed"
	}
	return fmt.Sprintf("%.3f%%", 100*float64(part)/float64(whole))
}

func saturatingSub(a, b uint64) uint64 {
	if b > a {
		return 0
	}
	return a - b
}

func mib(bytes uint64) string { return fmt.Sprintf("%.0f MiB", float64(bytes)/(1<<20)) }
