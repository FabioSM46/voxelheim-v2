package world

import "testing"

func TestValueNoiseIsDeterministic(t *testing.T) {
	t.Parallel()

	const seed = 0x1234
	for x := int64(0); x < 8*one; x += one / 7 {
		for y := int64(-3 * one); y < 3*one; y += one / 5 {
			first := valueNoise2D(seed, x, y)
			if second := valueNoise2D(seed, x, y); first != second {
				t.Fatalf("valueNoise2D(%d, %d) returned %d then %d", x, y, first, second)
			}

			z := (x - y) / 3
			first3D := valueNoise3D(seed, x, y, z)
			if second := valueNoise3D(seed, x, y, z); first3D != second {
				t.Fatalf("valueNoise3D(%d, %d, %d) returned %d then %d", x, y, z, first3D, second)
			}
		}
	}
}

func TestNoiseStaysInRange(t *testing.T) {
	t.Parallel()

	for x := int64(-5 * one); x < 5*one; x += one / 13 {
		if v := valueNoise2D(7, x, x/3); v < 0 || v > one {
			t.Fatalf("valueNoise2D produced %d, outside [0, %d]", v, one)
		}
		if v := fbm2D(7, x, x/3); v < 0 || v > one {
			t.Fatalf("fbm2D produced %d, outside [0, %d]", v, one)
		}
		if v := valueNoise3D(7, x, x/3, -x/5); v < 0 || v > one {
			t.Fatalf("valueNoise3D produced %d, outside [0, %d]", v, one)
		}
		if v := fbm3D(7, x, x/3, -x/5); v < 0 || v > one {
			t.Fatalf("fbm3D produced %d, outside [0, %d]", v, one)
		}
	}
}

func TestSmoothstepEndpointsAndMonotonicity(t *testing.T) {
	t.Parallel()

	if got := smoothstep(0); got != 0 {
		t.Errorf("smoothstep(0) = %d, want 0", got)
	}
	if got := smoothstep(one); got != one {
		t.Errorf("smoothstep(one) = %d, want %d", got, one)
	}

	previous := int64(-1)
	for t2 := int64(0); t2 <= one; t2 += one / 256 {
		v := smoothstep(t2)
		if v < previous {
			t.Fatalf("smoothstep is not monotonic: %d then %d", previous, v)
		}
		previous = v
	}
}

// A different seed must give a different field, or the world seed is decorative.
func TestSeedChangesTheField(t *testing.T) {
	t.Parallel()

	same2D, same3D := 0, 0
	const samples = 64
	for i := range samples {
		x := int64(i) * one / 3
		if valueNoise2D(1, x, x) == valueNoise2D(2, x, x) {
			same2D++
		}
		if valueNoise3D(1, x, x, -x/2) == valueNoise3D(2, x, x, -x/2) {
			same3D++
		}
	}
	if same2D > samples/8 {
		t.Errorf("%d of %d 2D samples matched across seeds; the seed is barely doing anything", same2D, samples)
	}
	if same3D > samples/8 {
		t.Errorf("%d of %d 3D samples matched across seeds; the seed is barely doing anything", same3D, samples)
	}
}

// Value noise interpolated with smoothstep has no jumps: adjacent samples differ
// by a small fraction of the range. A crease here means the interpolation or the
// lattice indexing is wrong, which reads as terraces in the terrain.
func TestNoiseIsContinuous(t *testing.T) {
	t.Parallel()

	const step = one / 64
	previous := valueNoise2D(3, 0, 0)
	previous3D := valueNoise3D(3, 0, one/3, -one/2)
	for x := int64(step); x < 6*one; x += step {
		v := valueNoise2D(3, x, one/2)
		if delta := v - previous; delta > one/8 || delta < -one/8 {
			t.Fatalf("noise jumped by %d between x=%d and x=%d", delta, x-step, x)
		}
		previous = v

		v3D := valueNoise3D(3, x, one/3, -one/2)
		if delta := v3D - previous3D; delta > one/8 || delta < -one/8 {
			t.Fatalf("3D noise jumped by %d between x=%d and x=%d", delta, x-step, x)
		}
		previous3D = v3D
	}
}

func TestNoiseIsNotConstant(t *testing.T) {
	t.Parallel()

	first := valueNoise2D(5, 0, 0)
	varies := false
	for x := int64(0); x < 20*one; x += one {
		if valueNoise2D(5, x, 0) != first {
			varies = true
			break
		}
	}
	if !varies {
		t.Fatal("the noise field is constant")
	}

	first3D := valueNoise3D(5, 0, 0, 0)
	varies = false
	for z := int64(0); z < 20*one; z += one {
		if valueNoise3D(5, 0, 0, z) != first3D {
			varies = true
			break
		}
	}
	if !varies {
		t.Fatal("the 3D noise field is constant")
	}
}
