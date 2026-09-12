//! Pins of rendered samples: a change to the synthesiser that is meant to change no sound is
//! shown to change none, to a millionth of full scale. Each catalogue keeps its own table
//! beside its own descriptions, so a change that is meant to alter one sound refreshes that
//! sound's row and the rest of the table still stands as the proof that nothing else moved.
//!
//! **Why not the raw bits.** The oscillators, the noise pole and the glide go through libm's
//! `sin`, `exp` and `powf`, and libm is not correctly rounded, nor the same between glibc,
//! Apple's and MSVC's, nor always between two versions of one. Raw bits would pin one
//! platform's last places: a table taken on Linux would report rows moved on macOS with no
//! synthesiser change, which is exactly the signal these pins exist to keep meaningful. A grid
//! of 2^-20 is sixteen f32 steps even at the top octave, so a last-place difference almost
//! never crosses a line on it, while any change a synthesiser can make to a sound — a gain, a
//! frequency, a sample of a phase — is thousands of steps wide. It also makes `-0.0` and `0.0`
//! one value, since they are one sound.

use super::Sound;

/// The rates every pin is rendered at: the lowest the synthesiser accepts and the two a
/// device most often opens at. A difference that shows only at one rate still shows.
pub(crate) const RATES: [u32; 3] = [8_000, 44_100, 48_000];

/// Steps of the grid a sample is read on, per unit of full scale: about -120 dBFS each.
const STEPS: f64 = (1u32 << 20) as f64;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fold(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// A sample as the nearest step of the grid. Every baked or streamed sample is clamped to
/// full scale, so the step fits an `i32` with room to spare.
fn canonical(sample: f32) -> i32 {
    (f64::from(sample) * STEPS).round() as i32
}

/// FNV-1a over the length and every sample's [`canonical`] step, at each of [`RATES`].
pub(crate) fn across_rates(mut render: impl FnMut(u32) -> Vec<f32>) -> u64 {
    RATES.iter().fold(FNV_OFFSET, |hash, &rate| {
        let samples = render(rate);
        let hash = fold(hash, &(samples.len() as u64).to_le_bytes());
        samples.iter().fold(hash, |hash, sample| {
            fold(hash, &canonical(*sample).to_le_bytes())
        })
    })
}

/// A baked sound's pin, at the duration and seed its player bakes it with.
pub(crate) fn baked(sound: &Sound, seconds: f32, seed: u64) -> u64 {
    across_rates(|rate| {
        sound
            .bake(seconds, rate, seed)
            .expect("a catalogued sound bakes")
            .samples()
            .to_vec()
    })
}

/// A continuous source's pin, over its first `seconds`.
pub(crate) fn continuous(sound: &Sound, seconds: f32, seed: u64) -> u64 {
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

/// Fails with the whole table as it renders now, ready to paste, and names each row that
/// moved, so a deliberate change refreshes exactly the rows it meant to.
pub(crate) fn assert_pins(rendered: &[(String, u64)], pinned: &[(&str, u64)]) {
    let names_match = rendered.len() == pinned.len()
        && rendered
            .iter()
            .zip(pinned)
            .all(|((name, _), (pin, _))| name == pin);
    let moved: Vec<&str> = rendered
        .iter()
        .zip(pinned)
        .filter(|((_, hash), (_, pin))| hash != pin)
        .map(|((name, _), _)| name.as_str())
        .collect();
    if names_match && moved.is_empty() {
        return;
    }
    let table: String = rendered
        .iter()
        .map(|(name, hash)| format!("    (\"{name}\", 0x{hash:016x}),\n"))
        .collect();
    panic!(
        "rendered samples differ from their pins (rows named alike: {names_match}; moved: \
         {moved:?}). Refresh only the rows a change is meant to alter. Rendered now:\n{table}"
    );
}

#[test]
fn a_pin_ignores_last_place_arithmetic_and_the_sign_of_zero_but_not_a_real_change() {
    // Samples on the grid's own steps, across full scale in both directions, and a zero.
    let steps: Vec<f32> = (-500..=500)
        .map(|k| (f64::from(k * 2000) / STEPS) as f32)
        .collect();
    let pin = |samples: &[f32]| across_rates(|_| samples.to_vec());
    let original = pin(&steps);
    // One f32 step up and one down on every sample: what a different libm leaves behind.
    for nudge in [f32::next_up, f32::next_down] {
        let nudged: Vec<f32> = steps.iter().map(|x| nudge(*x)).collect();
        assert_ne!(nudged, steps);
        assert_eq!(pin(&nudged), original);
    }
    let signed: Vec<f32> = steps
        .iter()
        .map(|x| if *x == 0.0 { -0.0 } else { *x })
        .collect();
    assert!(signed.iter().any(|x| x.is_sign_negative() && *x == 0.0));
    assert_eq!(pin(&signed), original);
    // The smallest change a sound could make — one sample, half a thousandth of a decibel of
    // gain on it — is still a moved row.
    let mut changed = steps.clone();
    changed[700] *= 1.0001;
    assert_ne!(pin(&changed), original);
}
