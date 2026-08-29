package world

import (
	"context"
	"reflect"
	"testing"
)

func TestRegenerationChunksUnitesStoredUnsavedAndResidentGround(t *testing.T) {
	t.Parallel()

	store, err := OpenStore(t.TempDir(), 7)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	stored := Coord{X: -2, Y: 0, Z: 1}
	writer := NewPersistentCache(store, 1, 8)
	x := int64(stored.X) * ChunkSize
	z := int64(stored.Z) * ChunkSize
	if err := writer.Apply(context.Background(), x, 1, z, Stone, func(Block) error { return nil }); err != nil {
		t.Fatalf("Apply stored edit: %v", err)
	}
	if err := writer.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}

	cache := NewPersistentCache(store, 1, 8)
	resident := Coord{X: 3, Y: 0, Z: -1}
	if _, _, err := cache.Get(context.Background(), resident); err != nil {
		t.Fatalf("Get resident: %v", err)
	}
	unsaved := Coord{X: 1, Y: 0, Z: 2}
	x = int64(unsaved.X) * ChunkSize
	z = int64(unsaved.Z) * ChunkSize
	if err := cache.Apply(context.Background(), x, 1, z, Stone, func(Block) error { return nil }); err != nil {
		t.Fatalf("Apply unsaved edit: %v", err)
	}

	got, err := cache.RegenerationChunks()
	if err != nil {
		t.Fatalf("RegenerationChunks: %v", err)
	}
	want := []Coord{stored, unsaved, resident}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("RegenerationChunks = %+v, want %+v", got, want)
	}
}
