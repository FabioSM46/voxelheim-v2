package main

import (
	"math"
	"time"
)

// A fixed-shape latency histogram, and the reason it is not a slice of samples.
//
// At a thousand sessions in one cluster, a hundred speakers relaying to nine hundred and
// ninety-nine listeners fifty times a second is about five million receipts a second. One
// time.Duration per receipt would allocate gigabytes and measure this command's allocator
// rather than the server's relay; a bucketed count is a few kilobytes per session and one
// increment per frame.
//
// **Two resolutions rather than one**, because the interesting numbers are three orders of
// magnitude apart: a relay on loopback is tens of microseconds and a relay that has gone
// wrong is hundreds of milliseconds. A single resolution fine enough for the first is a
// megabyte per session, and one coarse enough for the second reports p50 as "under a
// millisecond", which is the whole answer thrown away.
// tiers are the resolutions the histogram is built out of, coarsest last.
//
// **Three of them rather than one, because the interesting numbers are four orders of
// magnitude apart.** A relay on a quiet server is tens of microseconds; the same relay on a
// server whose tick has collapsed is tens of seconds. One resolution fine enough for the
// first needs three million buckets to reach the second, and one coarse enough for the
// second answers every healthy p50 with "under a tenth of a second".
//
// The tiers are contiguous, each spanning its step times its bucket count: 0..10 ms at ten
// microseconds, 10 ms..1 s at a millisecond, 1 s..31 s at a tenth of a second.
var tiers = [...]struct {
	step    time.Duration
	buckets int
}{
	{10 * time.Microsecond, 1000},
	{time.Millisecond, 990},
	{100 * time.Millisecond, 300},
}

// histogramCeiling is the longest latency the buckets can name, and overflowBucket holds
// everything at or past it. A percentile that lands there is reported as a bound and never
// as a number this command made up.
var histogramCeiling, overflowBucket = func() (time.Duration, int) {
	var ceiling time.Duration
	var buckets int
	for _, tier := range tiers {
		ceiling += tier.step * time.Duration(tier.buckets)
		buckets += tier.buckets
	}
	return ceiling, buckets
}()

// histogramBuckets is fixed at build time because the counts are an array on every session:
// a slice would be a per-session allocation and a bounds check on the hot path. It is
// checked against the tiers by the tests rather than derived from them, which is the one
// place in this file a number is written twice.
const histogramBuckets = 1000 + 990 + 300 + 1

type histogram struct {
	counts [histogramBuckets]uint32

	// max is kept exactly, because the longest stall is the one number a bucket cannot
	// answer and the one a reader asks about first.
	max time.Duration
}

func (h *histogram) add(d time.Duration) {
	if d > h.max {
		h.max = d
	}
	h.counts[bucketOf(d)]++
}

// bucketOf is the placement rule, extracted so the test can state it rather than infer it.
//
// A negative duration lands in bucket zero. It is reachable — two goroutines reading
// time.Now() around a relay can produce one — and it is a receipt that happened, so
// discarding it would make the delivered count and the latency count disagree for a reason
// no reader could see.
func bucketOf(d time.Duration) int {
	if d < 0 {
		return 0
	}
	base := 0
	floor := time.Duration(0)
	for _, tier := range tiers {
		span := tier.step * time.Duration(tier.buckets)
		if d < floor+span {
			return base + int((d-floor)/tier.step)
		}
		base += tier.buckets
		floor += span
	}
	return overflowBucket
}

// merge folds another histogram into this one. Every session keeps its own and they are
// added up once, after the run, so nothing on the receive path touches shared memory.
func (h *histogram) merge(other *histogram) {
	for i, count := range other.counts {
		h.counts[i] += count
	}
	if other.max > h.max {
		h.max = other.max
	}
}

func (h *histogram) count() uint64 {
	var total uint64
	for _, count := range h.counts {
		total += uint64(count)
	}
	return total
}

// quantile is the smallest bucket ceiling at or below which the given fraction of samples
// fall, and whether the answer is a real number at all.
//
// **It returns the bucket's upper edge, not its middle**: a percentile read off a histogram
// is a bound, and rounding towards the middle invents precision the counts do not have.
// false means it landed in the overflow bucket, where the only honest answer is "at least
// the ceiling" and [histogram.max] is what to print.
//
// **The rank is rounded up and reached with `>=`, and both halves of that are the fix for
// #930's review.** It read `uint64(total × fraction)` and `seen > rank`, which skipped the
// bucket whose cumulative count *equalled* the rank: 99 samples at 35 µs and one at 3 s
// reported p99 as three seconds, when 99 of the 100 are at or below 40 µs. The two forms
// agree everywhere `total × fraction` is not a whole number — `trunc(x)+1` is `ceil(x)`
// for any other x — which is why a test asking for p99.5 of a hundred samples passed
// against the defect and why the one that catches it asks for p99 of exactly a hundred.
//
// This is the nearest-rank definition: the smallest value at or below which at least the
// requested fraction of the samples fall. A rank of zero is raised to one, because the
// smallest occupied bucket is the honest answer to "the 0th percentile" and no samples at
// all is answered by the total check above.
func (h *histogram) quantile(fraction float64) (time.Duration, bool) {
	total := h.count()
	if total == 0 {
		return 0, false
	}
	rank := uint64(math.Ceil(float64(total) * fraction))
	if rank == 0 {
		rank = 1
	}
	var seen uint64
	for bucket, count := range h.counts {
		seen += uint64(count)
		if seen >= rank {
			if bucket >= overflowBucket {
				return histogramCeiling, false
			}
			return bucketEdge(bucket), true
		}
	}
	// Unreachable: rank is at most total, and the loop walks every bucket the samples are
	// in. Kept because the compiler needs a return and because a silent wrong answer here
	// would be worse than the longest sample.
	return h.max, true
}

// bucketEdge is the upper edge of one bucket: the largest latency it can hold.
//
// A percentile read off a histogram is a bound, and the edge is the honest form of it.
// Rounding towards the middle of the bucket would be inventing precision the counts do not
// have, which is the one thing a number destined for an ADR must not do.
func bucketEdge(bucket int) time.Duration {
	floor := time.Duration(0)
	for _, tier := range tiers {
		if bucket < tier.buckets {
			return floor + tier.step*time.Duration(bucket+1)
		}
		bucket -= tier.buckets
		floor += tier.step * time.Duration(tier.buckets)
	}
	return histogramCeiling
}
