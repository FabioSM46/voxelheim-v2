package main

import (
	"fmt"
	"io"
	"sort"
)

// printPlan writes the layout the flags describe, without connecting anything.
//
// **The point of a plan is that it is checkable before a run is paid for.** A thousand
// sessions take a minute to join and a mistake in the geometry — a cluster that cannot hear
// itself, two clusters that are one — is not visible in the numbers afterwards, only in the
// difference between what was owed and what arrived, which is the same shape as a drop.
func printPlan(out io.Writer, o options) error {
	l := &lines{to: out}
	places := planPlacements(o)
	origin := o.origin()

	l.printf("plan: %d sessions in %d cluster(s), seed %d, spawn %d %d %d\n",
		o.sessions, o.clusters, o.seed, origin[0], origin[1], origin[2])
	l.printf("  voice range %v blocks, cluster radius %d, cluster spacing %d\n",
		o.voiceRange, o.clusterRadius, o.clusterSpacing)
	l.printf("  %d frames/s of %d-byte opus from each speaker, window %v\n",
		o.frameRate, o.opusBytes, o.duration)
	l.printf("  %s\n\n", describeFrame(o.opusBytes))

	speakers := 0
	for cluster := range o.clusters {
		size := clusterSize(o.sessions, o.clusters, cluster)
		clusterSpeakers := speakersPerCluster(size, o.speaking)
		speakers += clusterSpeakers
		l.printf("  cluster %d: %d sessions, %d speaking, %d frames/s relayed to %d listeners each\n",
			cluster, size, clusterSpeakers, o.frameRate, size-1)
	}
	l.printf("\n  %d speakers owe %d deliveries a second in total\n", speakers, owedPerSecond(o))
	l.printf("  furthest apart inside a cluster: %d blocks, of %v carried\n", 2*o.clusterRadius, o.voiceRange)
	l.printf("  distinct blocks stood on: %d, sessions: %d\n", distinctBlocks(places), len(places))
	return l.err
}

// owedPerSecond is the delivery rate the plan asks the relay for.
//
// The number worth reading before starting a run: it is what the server has to encode once
// and hand to a session queue that many times, every second, under the simulation's own lock.
func owedPerSecond(o options) int {
	owed := 0
	for cluster := range o.clusters {
		size := clusterSize(o.sessions, o.clusters, cluster)
		owed += speakersPerCluster(size, o.speaking) * o.frameRate * (size - 1)
	}
	return owed
}

// distinctBlocks says how many whole blocks the plan actually occupies, which is how a
// reader sees that a crowd has wrapped around its lattice and is standing several deep.
func distinctBlocks(places []placement) int {
	keys := make([]string, 0, len(places))
	seen := make(map[string]struct{}, len(places))
	for _, place := range places {
		key := fmt.Sprintf("%d/%d/%d", place.at[0], place.at[1], place.at[2])
		if _, already := seen[key]; already {
			continue
		}
		seen[key] = struct{}{}
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return len(keys)
}

// describeFrame says what one synthetic frame is, in the plan, before a run is paid for.
//
// It builds the frame rather than describing it from the flags, so a size the option set
// accepted and the builder cannot make is a sentence here instead of a failure thirty
// seconds into a run.
func describeFrame(size int) string {
	packet, err := silenceFrame(size)
	if err != nil {
		return fmt.Sprintf("the frame: %v", err)
	}
	return fmt.Sprintf(
		"the frame: %d bytes of Opus — a %d-byte header, %d of padding, and the last %d of that padding the send instant the relay latency is measured from",
		len(packet), opusSilenceHeaderBytes, size-opusSilenceHeaderBytes, opusStampBytes)
}

// lines is the one place this command writes a report to, and it keeps the first error a
// write returned instead of asking every call site to.
//
// **A report is a hundred writes and one outcome.** Checking each one turns the report into
// a ladder of error handling that says nothing a reader wants; ignoring each one is what
// errcheck is right to refuse. bufio.Writer's sticky error is the shape this borrows: once
// a write has failed, the rest are skipped and the failure is answered once, by whoever
// asked for the report.
type lines struct {
	to  io.Writer
	err error
}

func (l *lines) printf(format string, args ...any) {
	if l.err != nil {
		return
	}
	_, l.err = fmt.Fprintf(l.to, format, args...)
}
