package world

import (
	"errors"
	"slices"
	"testing"
)

func uniformChunk(b Block) *Chunk {
	c := NewChunk(Coord{})
	for i := range c.Blocks {
		c.Blocks[i] = b
	}
	return c
}

// The worst case for run-length encoding: every voxel breaks the run.
func alternatingChunk() *Chunk {
	c := NewChunk(Coord{})
	for i := range c.Blocks {
		if i%2 == 0 {
			c.Blocks[i] = Stone
		} else {
			c.Blocks[i] = Air
		}
	}
	return c
}

func TestEncodeDecodeRoundTrip(t *testing.T) {
	t.Parallel()

	chunks := map[string]*Chunk{
		"all air":     uniformChunk(Air),
		"all stone":   uniformChunk(Stone),
		"terrain":     Generate(0x5EED, Coord{X: 3, Y: 2, Z: -5}),
		"alternating": alternatingChunk(),
	}

	for name, chunk := range chunks {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			pairs := Encode(chunk)
			blocks, err := Decode(pairs)
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}
			if !slices.Equal(blocks, chunk.Blocks) {
				t.Error("the chunk did not survive the round trip")
			}
		})
	}
}

// A uniform chunk is the best case and pins the encoding's shape: 32768 voxels of
// one block are one pair, and a single uint16 run length can hold them because
// 32³ < 65535. chunk.go carries the compile-time guard for that.
func TestUniformChunkEncodesToOnePair(t *testing.T) {
	t.Parallel()

	pairs := Encode(uniformChunk(Stone))
	if len(pairs) != 2 {
		t.Fatalf("a uniform chunk encoded to %d values, want 2", len(pairs))
	}
	if Block(pairs[0]) != Stone || int(pairs[1]) != ChunkVolume {
		t.Errorf("encoded (%d, %d), want (Stone, %d)", pairs[0], pairs[1], ChunkVolume)
	}
}

// The size assertion is the reason the encoding exists: 32³ voxels are 64 KiB raw,
// and streaming a view distance of 3 raw would be 22 MiB per join. A regression in
// the encoder shows up here rather than as a slow connection.
func TestTerrainChunkEncodesUnderTheCeiling(t *testing.T) {
	t.Parallel()

	const ceilingBytes = 4096

	for _, coord := range []Coord{{X: 0, Y: 2, Z: 0}, {X: 3, Y: 2, Z: -5}, {X: -12, Y: 1, Z: 7}} {
		pairs := Encode(Generate(0x5EED, coord))
		bytes := len(pairs) * 2
		if bytes > ceilingBytes {
			t.Errorf("chunk %+v encoded to %d bytes, over the %d ceiling", coord, bytes, ceilingBytes)
		}
		if bytes >= ChunkVolume*2 {
			t.Errorf("chunk %+v encoded to %d bytes, no better than raw (%d)", coord, bytes, ChunkVolume*2)
		}
	}
}

// Correctness before compression: the pathological chunk must still round-trip,
// even though its encoding is larger than the raw blocks.
func TestAlternatingChunkIsCorrectEvenThoughLarger(t *testing.T) {
	t.Parallel()

	chunk := alternatingChunk()
	pairs := Encode(chunk)

	if len(pairs) != 2*ChunkVolume {
		t.Fatalf("alternating chunk encoded to %d values, want %d", len(pairs), 2*ChunkVolume)
	}
	blocks, err := Decode(pairs)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if !slices.Equal(blocks, chunk.Blocks) {
		t.Error("the alternating chunk did not survive the round trip")
	}
}

// Every invariant schemas/world.fbs documents, refused. A decoder that trusted the
// payload would either allocate whatever it was told to or hand a half-filled
// chunk to the mesher.
func TestDecodeRejectsMalformedPayloads(t *testing.T) {
	t.Parallel()

	full := uint16(ChunkVolume)

	cases := map[string][]uint16{
		"empty":                    {},
		"odd length":               {uint16(Stone), full, uint16(Air)},
		"zero-length run":          {uint16(Stone), 0, uint16(Air), full},
		"too few voxels":           {uint16(Stone), full - 1},
		"too many voxels":          {uint16(Stone), full, uint16(Air), 1},
		"single run overshoots":    {uint16(Stone), full, uint16(Stone), full},
		"runs sum past the volume": {uint16(Stone), 40000, uint16(Air), 40000},
	}

	for name, pairs := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if _, err := Decode(pairs); !errors.Is(err, ErrMalformedRuns) {
				t.Fatalf("Decode(%s) error = %v, want ErrMalformedRuns", name, err)
			}
		})
	}
}
