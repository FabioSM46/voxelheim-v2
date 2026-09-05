package main

import (
	"testing"
	"time"
)

func TestABucketIsChosenByTheBandTheLatencyFallsIn(t *testing.T) {
	t.Parallel()

	cases := []struct {
		latency time.Duration
		bucket  int
	}{
		{-1 * time.Millisecond, 0},
		{0, 0},
		{9 * time.Microsecond, 0},
		{10 * time.Microsecond, 1},
		{10*time.Millisecond - time.Microsecond, 999},
		{10 * time.Millisecond, 1000},
		{11 * time.Millisecond, 1001},
		{time.Second - time.Microsecond, 1989},
		{time.Second, 1990},
		{1100 * time.Millisecond, 1991},
		{31*time.Second - time.Microsecond, 2289},
		{31 * time.Second, overflowBucket},
		{time.Hour, overflowBucket},
	}
	for _, c := range cases {
		if got := bucketOf(c.latency); got != c.bucket {
			t.Errorf("bucketOf(%v) is %d, want %d", c.latency, got, c.bucket)
		}
	}
}

func TestAQuantileIsTheBucketsUpperEdgeAndSaysWhenItIsOnlyABound(t *testing.T) {
	t.Parallel()

	var h histogram
	if _, ok := h.quantile(0.5); ok {
		t.Error("an empty histogram answered a quantile")
	}

	for range 99 {
		h.add(35 * time.Microsecond)
	}
	h.add(3 * time.Second)

	if got := h.count(); got != 100 {
		t.Fatalf("the histogram counted %d samples, want 100", got)
	}
	if got := h.max; got != 3*time.Second {
		t.Errorf("the longest sample is %v, want 3s", got)
	}

	// 35 µs falls in the bucket that ends at 40 µs, and the answer is that edge rather
	// than a number invented inside it.
	p50, ok := h.quantile(0.5)
	if !ok || p50 != 40*time.Microsecond {
		t.Errorf("p50 is %v (exact=%v), want 40µs exact", p50, ok)
	}
	// 3 s is inside the coarsest tier, so it is a number rather than a bound.
	if p99, ok := h.quantile(0.995); !ok || p99 != 3100*time.Millisecond {
		t.Errorf("p99.5 is %v (exact=%v), want 3.1s exact", p99, ok)
	}

	var beyond histogram
	beyond.add(2 * time.Hour)
	if _, ok := beyond.quantile(0.5); ok {
		t.Error("a quantile that landed in the overflow bucket was reported as exact")
	}
}

// The array the counts live in is written down, so the one place two numbers describe the
// same thing is the one place a test has to hold them together.
func TestTheBucketArrayIsTheSizeTheTiersNeed(t *testing.T) {
	t.Parallel()

	total := 1
	for _, tier := range tiers {
		total += tier.buckets
	}
	if total != histogramBuckets {
		t.Fatalf("the tiers need %d buckets and the array holds %d", total, histogramBuckets)
	}
	if overflowBucket != histogramBuckets-1 {
		t.Errorf("the overflow bucket is %d, not the last of %d", overflowBucket, histogramBuckets)
	}
	if histogramCeiling != 31*time.Second {
		t.Errorf("the tiers reach %v, want 31s", histogramCeiling)
	}
	if got := bucketEdge(overflowBucket); got != histogramCeiling {
		t.Errorf("the overflow bucket's edge is %v, want the ceiling %v", got, histogramCeiling)
	}
}

func TestMergingKeepsEveryCountAndTheLongestSample(t *testing.T) {
	t.Parallel()

	var a, b histogram
	a.add(20 * time.Microsecond)
	b.add(20 * time.Microsecond)
	b.add(500 * time.Millisecond)
	a.merge(&b)

	if got := a.count(); got != 3 {
		t.Errorf("the merged histogram holds %d samples, want 3", got)
	}
	if a.max != 500*time.Millisecond {
		t.Errorf("the merged longest sample is %v, want 500ms", a.max)
	}
}
