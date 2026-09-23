//! What the dungeon's moving parts sound like, as descriptions for the synth.
//!
//! **Every one of them is shaped noise.** A lever is iron teeth over a ratchet and a heavy
//! stop; a rune waking is stone dragged over stone with a ring in it; a grille is loose bars
//! chattering in their channel; a door is a slab ground along its sill; a web is silk fibres
//! parting one after another. None of those is a note, and the only way any of them is
//! made here is by filtering, gating and enveloping noise. The rune's ring is the nearest
//! thing to a pitch, and it is a resonance — noise through a narrow band that decays — not
//! an oscillator, so it has the shimmer of struck stone rather than the purity of a hum.
//!
//! `tests.rs` holds each of them to a spectral flatness a pure tone cannot reach, and runs
//! the same measure over each description with its noise replaced by sines to show the
//! measure would catch one.

use crate::audio::synth::{
    Curve, Envelope, Exciter, Filter, FilterKind, Gate, Layer, Noise, Sound,
};

/// One moving part of the dungeon, heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cue {
    /// A lever thrown, either way: a ratchet over iron teeth and a clunk at the stop.
    Lever,
    /// A rune stone lit: a stone scrape with a resonant ring blooming in it.
    RuneIgnite,
    /// A lit rune stone going dark: the scrape alone, lower, with nothing ringing.
    RuneDouse,
    /// A grille winding up out of the way: bars chattering, slowing as it rises.
    GrilleUp,
    /// A grille dropping shut: faster chatter that ends in a heavy iron stop.
    GrilleDown,
    /// A stone door grinding along its sill, open or shut.
    Door,
    /// A web torn apart.
    WebTear,
}

#[cfg(test)]
pub(super) const CUES: [Cue; 7] = [
    Cue::Lever,
    Cue::RuneIgnite,
    Cue::RuneDouse,
    Cue::GrilleUp,
    Cue::GrilleDown,
    Cue::Door,
    Cue::WebTear,
];

fn envelope(attack: f32, decay: f32) -> Envelope {
    Envelope {
        attack,
        decay,
        sustain: 0.0,
        release: 0.01,
    }
}

/// Noise through a filter of `kind` at `hz`: the one texture every cue is built from.
fn noise(noise: Noise, kind: FilterKind, hz: f32, q: f32, gain: f32, env: Envelope) -> Layer {
    Layer {
        exciter: Exciter::Noise(noise),
        gain,
        envelope: env,
        gate: None,
        filter: Some(Filter { kind, hz, q }),
    }
}

/// Band-passed white noise.
fn band(hz: f32, q: f32, gain: f32, attack: f32, decay: f32) -> Layer {
    noise(
        Noise::White,
        FilterKind::Band,
        hz,
        q,
        gain,
        envelope(attack, decay),
    )
}

/// Low-passed brown noise: the weight under a heavy thing moving.
fn weight(hz: f32, gain: f32, attack: f32, decay: f32) -> Layer {
    noise(
        Noise::Brown,
        FilterKind::Low,
        hz,
        0.7,
        gain,
        envelope(attack, decay),
    )
}

/// A layer struck open and shut `from`–`to` times a second along `curve`: one impact per
/// opening, for a ratchet, a chatter or a tear.
fn struck(layer: Layer, from: f32, to: f32, seconds: f32, duty: f32, curve: Curve) -> Layer {
    Layer {
        gate: Some(Gate {
            from,
            to,
            seconds,
            curve,
            duty,
        }),
        ..layer
    }
}

impl Cue {
    /// How long the cue's buffer is.
    pub fn seconds(self) -> f32 {
        match self {
            Self::Lever => 0.42,
            Self::RuneIgnite => 1.2,
            Self::RuneDouse => 0.6,
            Self::GrilleUp => 1.4,
            Self::GrilleDown => 0.9,
            Self::Door => 2.2,
            Self::WebTear => 0.34,
        }
    }

    pub fn describe(self) -> Sound {
        let layers = match self {
            Self::Lever => vec![
                // The ratchet: a pawl clicking over iron teeth, a few times and slowing.
                struck(
                    band(2300.0, 2.2, 0.55, 0.002, 0.26),
                    26.0,
                    14.0,
                    0.26,
                    0.22,
                    Curve::Linear,
                ),
                struck(
                    band(1300.0, 1.6, 0.36, 0.002, 0.26),
                    19.0,
                    13.0,
                    0.26,
                    0.18,
                    Curve::Linear,
                ),
                // The stop: iron meeting its seat, a dull knock and the weight behind it.
                band(700.0, 1.1, 0.45, 0.004, 0.2),
                weight(260.0, 0.5, 0.004, 0.3),
                band(3100.0, 1.8, 0.2, 0.001, 0.05),
            ],
            Self::RuneIgnite => vec![
                // Stone over stone: a rough broadband drag with grit catching in it.
                band(850.0, 0.8, 0.46, 0.06, 0.8),
                struck(
                    band(2400.0, 1.3, 0.3, 0.05, 0.75),
                    47.0,
                    29.0,
                    0.8,
                    0.4,
                    Curve::Linear,
                ),
                weight(300.0, 0.3, 0.05, 0.6),
                // The ring: two inharmonic stone resonances, noise through narrow bands,
                // blooming after the scrape has started and outlasting it.
                band(1330.0, 9.0, 0.34, 0.18, 1.0),
                band(2170.0, 9.5, 0.2, 0.22, 0.95),
            ],
            Self::RuneDouse => vec![
                band(620.0, 0.8, 0.44, 0.04, 0.5),
                struck(
                    band(1900.0, 1.2, 0.26, 0.04, 0.5),
                    38.0,
                    22.0,
                    0.55,
                    0.4,
                    Curve::Linear,
                ),
                weight(240.0, 0.32, 0.04, 0.45),
            ],
            Self::GrilleUp => vec![
                // Bars chattering in their channel as they are hauled up, slowing near the top.
                struck(
                    band(2900.0, 2.4, 0.46, 0.03, 1.3),
                    24.0,
                    11.0,
                    1.3,
                    0.3,
                    Curve::Exponential,
                ),
                struck(
                    band(1700.0, 1.8, 0.36, 0.03, 1.3),
                    17.0,
                    9.0,
                    1.3,
                    0.28,
                    Curve::Exponential,
                ),
                // The chain winding: a steady looser rattle, and the iron's weight.
                struck(
                    band(1100.0, 1.2, 0.24, 0.05, 1.25),
                    31.0,
                    27.0,
                    1.3,
                    0.5,
                    Curve::Linear,
                ),
                weight(200.0, 0.3, 0.1, 1.2),
            ],
            Self::GrilleDown => vec![
                // Dropping: the chatter quickens as it falls...
                struck(
                    band(2700.0, 2.4, 0.42, 0.01, 0.55),
                    14.0,
                    34.0,
                    0.55,
                    0.3,
                    Curve::Exponential,
                ),
                struck(
                    band(1500.0, 1.8, 0.32, 0.01, 0.55),
                    11.0,
                    26.0,
                    0.55,
                    0.26,
                    Curve::Exponential,
                ),
                // ...and ends in the heavy stop of iron on stone, felt more than heard.
                weight(180.0, 0.6, 0.4, 0.45),
                band(600.0, 1.0, 0.36, 0.42, 0.4),
            ],
            Self::Door => vec![
                // A slab ground along its sill: a low rumble with a rough flutter in it...
                weight(220.0, 0.55, 0.2, 1.9),
                struck(
                    band(420.0, 1.1, 0.5, 0.2, 1.9),
                    9.0,
                    6.5,
                    2.1,
                    0.8,
                    Curve::Linear,
                ),
                // ...and grit crushed under it, crackling unevenly.
                struck(
                    band(1250.0, 0.9, 0.3, 0.2, 1.85),
                    37.0,
                    29.0,
                    2.1,
                    0.4,
                    Curve::Linear,
                ),
                struck(
                    band(2600.0, 1.4, 0.16, 0.2, 1.8),
                    53.0,
                    41.0,
                    2.1,
                    0.25,
                    Curve::Linear,
                ),
            ],
            Self::WebTear => vec![
                // Fibres parting one after another, quicker as the tear runs away.
                struck(
                    band(3300.0, 1.2, 0.5, 0.004, 0.3),
                    60.0,
                    110.0,
                    0.3,
                    0.45,
                    Curve::Exponential,
                ),
                struck(
                    band(2100.0, 1.0, 0.34, 0.004, 0.3),
                    47.0,
                    83.0,
                    0.3,
                    0.4,
                    Curve::Exponential,
                ),
                // The dry rasp of the silk itself, under the snapping.
                band(2800.0, 0.7, 0.22, 0.02, 0.26),
            ],
        };
        Sound { layers }
    }
}
