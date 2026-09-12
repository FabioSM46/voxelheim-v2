//! Pins of rendered samples: a change to the synthesiser that is meant to change no sound is
//! shown to change none, bit for bit. Each catalogue keeps its own table beside its own
//! descriptions, so a change that is meant to alter one sound refreshes that sound's row and
//! the rest of the table still stands as the proof that nothing else moved.

use super::Sound;

/// The rates every pin is rendered at: the lowest the synthesiser accepts and the two a
/// device most often opens at. A difference that shows only at one rate still shows.
pub(crate) const RATES: [u32; 3] = [8_000, 44_100, 48_000];

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fold(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// FNV-1a over the length and every sample's IEEE-754 bits, at each of [`RATES`]. Bits, not
/// values: a sample that moved in its last place is a different sound for this purpose.
pub(crate) fn across_rates(mut render: impl FnMut(u32) -> Vec<f32>) -> u64 {
    RATES.iter().fold(FNV_OFFSET, |hash, &rate| {
        let samples = render(rate);
        let hash = fold(hash, &(samples.len() as u64).to_le_bytes());
        samples.iter().fold(hash, |hash, sample| {
            fold(hash, &sample.to_bits().to_le_bytes())
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
