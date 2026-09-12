//! Pins of rendered samples: a change to the synthesiser that is meant to change no sound is
//! shown to change none. Each catalogue keeps its own table beside its own descriptions, so a
//! change that is meant to alter one sound refreshes that sound's row and the rest of the
//! table still stands as the proof that nothing else moved.
//!
//! **What a row is.** The number of samples rendered, and four projections of those samples
//! onto fixed pseudo-random sequences of +1 and -1, one sequence per projection, over the
//! renders at every one of [`RATES`] in turn. A row matches when the count is equal and every
//! projection is within [`SAMPLE_TOLERANCE`] times the count, plus the precision a table prints
//! a projection to.
//!
//! **Why a tolerance, and why not a hash.** The oscillators, the noise pole and the glide go
//! through libm's `sin`, `exp` and `powf`, which are not correctly rounded and are not the same
//! in glibc, Apple's libm and MSVC's. A hash of raw bits pins one libm's last places. Rounding
//! each sample to a grid before hashing only makes that rarer: a sample within one f32 step of
//! a rounding boundary still flips, and a row holds tens of thousands of samples. A projection
//! moves by at most the sum of how far each sample moved, so the tolerance is a guarantee
//! rather than a likelihood:
//!
//! - **A render whose every sample is within 2^-23 of the pinned one always matches.** 2^-23 is
//!   one f32 step at full scale, the widest step a sample can have.
//! - **A render with any single sample moved by more than the tolerance always moves its row.**
//! - **A change spread across the render is missed only if all four of its projections land
//!   inside the tolerance.** For a change unrelated to the sign sequences, with an RMS of ten
//!   times 2^-23 × √(samples), that is about one chance in 25,000.
//!
//! **What is not claimed.** That another libm keeps every sample within 2^-23. The argument
//! for it: a libm's last-place error reaches a sample as f64 noise around 1e-16, and nothing
//! in these filters or sums amplifies it by the 2^29 it would need. That is an argument, not a
//! measurement. Every table here was taken on one libm, glibc 2.39 on x86_64.

use super::Sound;

/// The rates every pin is rendered at: the lowest the synthesiser accepts and the two a
/// device most often opens at. A difference that shows only at one rate still shows.
pub(crate) const RATES: [u32; 3] = [8_000, 44_100, 48_000];

/// Projections per row. See the module documentation for what four buys.
pub(crate) const PROJECTIONS: usize = 4;

/// How far one sample may move and still be the same render: one f32 step at full scale.
const SAMPLE_TOLERANCE: f64 = 1.0 / (1u32 << 23) as f64;

/// The precision a table prints a projection to, which the comparison allows on top of the
/// tolerance. It also covers how differently two nearly equal sums of a million f64 terms
/// round, which is a millionth of this.
const PRINTED: f64 = 1e-5;

/// One rendered sound, reduced to what a table stores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pin {
    pub samples: u64,
    pub projections: [f64; PROJECTIONS],
}

/// A row of a table: the sound's name, its sample count and its printed projections.
pub(crate) type Row = (&'static str, u64, [f64; PROJECTIONS]);

/// +1 or -1 for sample `index` of projection `projection`: SplitMix64 over the pair, integer
/// arithmetic only, so every platform draws the same sequence.
fn sign(projection: usize, index: u64) -> f64 {
    let mut value = (index ^ ((projection as u64 + 1) << 56)).wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;
    if value >> 63 == 0 { 1.0 } else { -1.0 }
}

/// The pin of the samples `render` returns at each of [`RATES`], projected as one sequence.
pub(crate) fn across_rates(mut render: impl FnMut(u32) -> Vec<f32>) -> Pin {
    let mut pin = Pin {
        samples: 0,
        projections: [0.0; PROJECTIONS],
    };
    for rate in RATES {
        for sample in render(rate) {
            for (projection, sum) in pin.projections.iter_mut().enumerate() {
                *sum += sign(projection, pin.samples) * f64::from(sample);
            }
            pin.samples += 1;
        }
    }
    pin
}

/// A baked sound's pin, at the duration and seed its player bakes it with.
pub(crate) fn baked(sound: &Sound, seconds: f32, seed: u64) -> Pin {
    across_rates(|rate| {
        sound
            .bake(seconds, rate, seed)
            .expect("a catalogued sound bakes")
            .samples()
            .to_vec()
    })
}

/// A continuous source's pin, over its first `seconds`.
pub(crate) fn continuous(sound: &Sound, seconds: f32, seed: u64) -> Pin {
    across_rates(|rate| {
        let mut source = sound
            .continuous(rate, seed)
            .expect("a catalogued bed compiles");
        let mut samples = vec![0.0; (seconds * rate as f32) as usize];
        let written = source.render(&mut samples);
        samples.truncate(written);
        samples
    })
}

/// How far a render's worst projection is from the row, as a multiple of what the row allows:
/// at most 1 is a match. A different sample count never matches.
fn distance(pin: Pin, row: &Row) -> f64 {
    if pin.samples != row.1 {
        return f64::INFINITY;
    }
    let allowed = pin.samples as f64 * SAMPLE_TOLERANCE + PRINTED;
    pin.projections
        .iter()
        .zip(row.2)
        .map(|(rendered, pinned)| (rendered - pinned).abs() / allowed)
        .fold(0.0, f64::max)
}

/// Fails with the whole table as it renders now, ready to paste, and names each row that
/// moved and by how many tolerances, so a deliberate change refreshes exactly the rows it
/// meant to.
pub(crate) fn assert_pins(rendered: &[(String, Pin)], pinned: &[Row]) {
    let names_match = rendered.len() == pinned.len()
        && rendered
            .iter()
            .zip(pinned)
            .all(|((name, _), row)| name == row.0);
    let moved: Vec<String> = rendered
        .iter()
        .zip(pinned)
        .map(|((name, pin), row)| (name, distance(*pin, row)))
        .filter(|(_, distance)| *distance > 1.0)
        .map(|(name, distance)| format!("{name} ({distance:.1} tolerances)"))
        .collect();
    if names_match && moved.is_empty() {
        return;
    }
    let table: String = rendered
        .iter()
        .map(|(name, pin)| {
            let [a, b, c, d] = pin.projections;
            format!(
                "    (\"{name}\", {}, [{a:.5}, {b:.5}, {c:.5}, {d:.5}]),\n",
                pin.samples
            )
        })
        .collect();
    panic!(
        "rendered samples differ from their pins (rows named alike: {names_match}; moved: \
         {moved:?}). Refresh only the rows a change is meant to alter. Rendered now:\n{table}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples nowhere near any grid: a wandering waveform, with full scale in both directions
    /// (where an f32 step is widest), zeros and a value far below a step.
    fn render() -> Vec<f32> {
        let mut samples: Vec<f32> = (0..48_000)
            .map(|i| {
                let t = f64::from(i);
                (0.93 * (t * 0.0371).sin() * (t * 0.001_13).cos()) as f32
            })
            .collect();
        for (index, value) in [(10, 1.0), (20, -1.0), (30, 0.0), (40, 1e-9), (50, 0.0)] {
            samples[index] = value;
        }
        samples
    }

    /// The row a table would hold for `samples`: projections printed to [`PRINTED`].
    fn stored(samples: &[f32]) -> Row {
        let pin = across_rates(|_| samples.to_vec());
        let printed = pin.projections.map(|x| (x / PRINTED).round() * PRINTED);
        ("render", pin.samples, printed)
    }

    fn matches(samples: &[f32], row: &Row) -> bool {
        distance(across_rates(|_| samples.to_vec()), row) <= 1.0
    }

    #[test]
    fn a_render_whose_every_sample_moved_by_up_to_one_full_scale_step_still_matches() {
        let base = render();
        let row = stored(&base);
        assert!(matches(&base, &row));
        let nudged = |nudge: &dyn Fn(usize, f32) -> f32| -> Vec<f32> {
            base.iter()
                .enumerate()
                .map(|(index, x)| nudge(index, *x))
                .collect()
        };
        // One f32 step on every sample, up, down, and alternating.
        for samples in [
            nudged(&|_, x| x.next_up()),
            nudged(&|_, x| x.next_down()),
            nudged(&|index, x| {
                if index % 2 == 0 {
                    x.next_up()
                } else {
                    x.next_down()
                }
            }),
        ] {
            assert_ne!(samples, base);
            assert!(matches(&samples, &row));
        }
        // The worst case the bound allows, for each projection in turn: every sample moved
        // toward the tolerance, each in the direction that projection counts it. Three
        // quarters of a full-scale step, because the move is made in f64 and cast back: in
        // [0.5, 1) that cast rounds by up to 2^-25 more, which lands exactly on 2^-23.
        for projection in 0..PROJECTIONS {
            // The pin renders the samples once per rate, so each copy is moved by the signs
            // of its own place in the sequence.
            let pinned = across_rates(|_| base.clone());
            let moved = across_rates(|rate| {
                let offset = RATES.iter().position(|r| *r == rate).unwrap() * base.len();
                base.iter()
                    .enumerate()
                    .map(|(index, x)| {
                        let toward = sign(projection, (offset + index) as u64);
                        (f64::from(*x) + toward * SAMPLE_TOLERANCE * 0.75) as f32
                    })
                    .collect()
            });
            let shift = (moved.projections[projection] - pinned.projections[projection]).abs();
            assert!(
                shift > pinned.samples as f64 * SAMPLE_TOLERANCE * 0.5,
                "projection {projection} moved {shift}"
            );
            assert!(distance(moved, &row) <= 1.0, "projection {projection}");
        }
        // A zero's sign is no part of a sound.
        let signed = nudged(&|_, x| if x == 0.0 { -0.0 } else { x });
        assert!(signed.iter().any(|x| *x == 0.0 && x.is_sign_negative()));
        assert!(matches(&signed, &row));
    }

    #[test]
    fn a_change_past_the_tolerance_moves_the_row() {
        let base = render();
        let row = stored(&base);
        let allowed = (3 * base.len()) as f64 * SAMPLE_TOLERANCE + PRINTED;
        // One sample, moved by more than the whole tolerance: every projection sees all of it.
        let mut click = base.clone();
        click[24_000] += (allowed * 1.5) as f32;
        assert!(!matches(&click, &row));
        // The same render a thousandth louder, which is 0.009 dB.
        let louder: Vec<f32> = base.iter().map(|x| x * 1.001).collect();
        assert!(!matches(&louder, &row));
        // And a sample count that differs is never the same render.
        assert!(!matches(&base[1..], &row));
    }
}
