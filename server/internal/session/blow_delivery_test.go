package session

import (
	"context"
	"io"
	"log/slog"
	"reflect"
	"testing"
	"time"
)

func TestBlowMailboxReplacementKeepsOnlyTheMatchingSnapshot(t *testing.T) {
	snapshots := make(chan snapshotAt, 1)
	first := snapshotAt{frame: []byte{1}, following: [][]byte{{2}, {3}}}
	latest := snapshotAt{frame: []byte{4}, following: [][]byte{{5}}}
	if !offerLatestSnapshot(snapshots, first) || !offerLatestSnapshot(snapshots, latest) {
		t.Fatal("mailbox offer failed")
	}
	got := <-snapshots
	if !reflect.DeepEqual(got, latest) {
		t.Fatalf("replaced snapshot retained stale events: %+v", got)
	}
	if len(snapshots) != 0 {
		t.Fatal("mailbox retained duplicate")
	}
}

func TestBlowDeliveryOrderingFullQueuesAndWorldCancellation(t *testing.T) {
	for _, mode := range []string{"ordered", "snapshot-full", "event-full", "cancel-before", "cancel-after-snapshot"} {
		t.Run(mode, func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			snapshots := make(chan snapshotAt, 1)
			snapshots <- snapshotAt{frame: []byte{1}, following: [][]byte{{2}, {3}}}
			done := make(chan struct{})
			var offered []byte
			if mode == "cancel-before" {
				cancel()
			}
			go func() {
				defer close(done)
				followSnapshots(ctx, snapshots, func(frame []byte) bool {
					offered = append(offered, frame[0])
					if mode == "cancel-after-snapshot" {
						cancel()
					}
					if mode == "snapshot-full" {
						cancel()
						return false
					}
					if frame[0] == 3 {
						cancel()
					}
					if mode == "event-full" && frame[0] == 2 {
						return false
					}
					return true
				}, slog.New(slog.NewTextHandler(io.Discard, nil)))
			}()
			select {
			case <-done:
			case <-time.After(2 * time.Second):
				t.Fatal("snapshot worker did not end")
			}
			want := []byte{1, 2, 3}
			if mode == "snapshot-full" || mode == "cancel-after-snapshot" {
				want = []byte{1}
			}
			if mode == "cancel-before" {
				want = nil
			}
			if !reflect.DeepEqual(offered, want) {
				t.Fatalf("offers = %v, want %v", offered, want)
			}
		})
	}
}
