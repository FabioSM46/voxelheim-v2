//! Small descriptions rather than assets: every continuous layer advances fresh noise.
use crate::audio::synth::{
    self, Baked, Curve, Envelope, Exciter, Filter, FilterKind, Gate, Glide, Layer, Noise, Sound,
    Vibrato, Wave,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bed {
    Rain,
    DrivingRain,
    Snowfall,
    Sandstorm,
    Blizzard,
}

fn noise(noise: Noise, gain: f32, kind: FilterKind, hz: f32, q: f32) -> Layer {
    Layer {
        exciter: Exciter::Noise(noise),
        gain,
        envelope: Envelope {
            attack: 0.5,
            decay: 0.0,
            sustain: 1.0,
            release: 0.8,
        },
        gate: None,
        filter: Some(Filter { kind, hz, q }),
    }
}

impl Bed {
    pub(super) fn description(self) -> Sound {
        // All bands remain below the lowest supported sample rate's Nyquist margin.
        // Rain gains a second, lower wash as intensity grows: more than louder drizzle.
        let layers = match self {
            Self::Rain => vec![noise(Noise::White, 0.18, FilterKind::High, 1800.0, 0.7)],
            Self::DrivingRain => vec![noise(Noise::White, 0.3, FilterKind::Low, 950.0, 0.7)],
            // A soft granular hush, not the bright white-noise streaks of rainfall.
            Self::Snowfall => vec![noise(Noise::Brown, 0.12, FilterKind::Band, 380.0, 0.7)],
            // Sandy grit over a low wind; the blizzard is colder, narrower and higher.
            Self::Sandstorm => vec![
                noise(Noise::Brown, 0.6, FilterKind::Low, 550.0, 0.7),
                noise(Noise::White, 0.16, FilterKind::Band, 1300.0, 0.8),
            ],
            Self::Blizzard => vec![
                noise(Noise::White, 0.23, FilterKind::Band, 750.0, 2.8),
                noise(Noise::Brown, 0.32, FilterKind::Low, 230.0, 0.7),
            ],
        };
        Sound { layers }
    }
}

/// A voice, and nothing but a voice. These descriptions spawn no entities: snow's daytime
/// bird is the eagle already selected by birds::species_for, and the macaw's body comes from
/// `birds.rs`.
///
/// **Where each of these is heard, and when, is not here** — it is one row of
/// [`super::wildlife::WILDLIFE`], which is also where the rule about where a voice
/// originates is written down. This enum is the sound; that table is the creature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Call {
    Rattlesnake,
    /// The snow's daytime raptor: a harsh, descending scream, built by [`scream`] from
    /// [`EAGLE`].
    Eagle,
    /// The sand's daytime raptor, the voice of the griffon vulture `birds::BIRDS` already
    /// flies over the desert: the same construction as the eagle's and none of its numbers —
    /// lower, hoarser and sparser. Built by [`scream`] from [`CONDOR`].
    Condor,
    Wolf,
    /// Green country at night: an occasional cricket, not a continuous wall of them.
    Cricket,
    /// The macaw's squawk, heard by day only where `birds::species_for` answers the macaw.
    Parrot,
}

/// Content parameters for the existing Calls lane. Intervals exceed each sound's
/// duration by a wide margin; even dusk leaves the desert mostly silent.
pub(super) struct CallProfile {
    pub interval: [f32; 2],
    pub radius: f32,
    pub height: f32,
    pub seconds: f32,
    pub range: f32,
}

/// How many syllables one cricket call carries: two or three, from its seed — a "cri-cri".
pub(super) fn syllables(seed: u64) -> usize {
    2 + ((seed >> 8) % 2) as usize
}

/// How long one cricket syllable sounds.
pub(super) const SYLLABLE_SECONDS: f32 = 0.06;

/// Where each syllable of a call starts: one every 0.15 to 0.19 s, from its seed, so every
/// syllable is followed by at least 90 ms of silence before the next one.
pub(super) fn syllable_onsets(seed: u64) -> Vec<f32> {
    let period = 0.15 + ((seed >> 16) % 41) as f32 / 1000.0;
    (0..syllables(seed))
        .map(|index| index as f32 * period)
        .collect()
}

/// How many squawks one macaw call carries: one or two, from its seed — a "raak", or a
/// "raak... raak".
pub(super) fn squawks(seed: u64) -> usize {
    1 + ((seed >> 8) % 2) as usize
}

/// How long one squawk sounds.
pub(super) const SQUAWK_SECONDS: f32 = 0.3;

/// Where each squawk of a call starts: one every 0.42 to 0.52 s, from its seed, so a second
/// squawk follows the first after at least 120 ms of silence.
pub(super) fn squawk_onsets(seed: u64) -> Vec<f32> {
    let period = 0.42 + ((seed >> 16) % 101) as f32 / 1000.0;
    (0..squawks(seed))
        .map(|index| index as f32 * period)
        .collect()
}

/// How far the squawk's pitch arches above its falling line, as the vibrato's depth: one half
/// cycle of the vibrato spans the squawk, so the pitch rises into the call and falls out of it.
const SQUAWK_ARCH: f32 = 0.35;

/// Where the squawk's pitch line ends, as a fraction of where it starts.
const SQUAWK_FALL: f32 = 0.72;

/// How far above the voice its rough twin sits: at 620 to 780 Hz, 4% beats at 25 to 31 Hz,
/// the rattle of a harsh throat rather than a second note.
const SQUAWK_DETUNE: f32 = 1.04;

/// One squawk of a macaw: harsh, raspy and broadband, never a note.
///
/// - **A pitch contour.** Every voiced layer rides one glide from the seed's pitch down to
///   [`SQUAWK_FALL`] of it, lifted by a single half-cycle of vibrato that peaks mid-squawk:
///   the pitch rises about 15% into the call and then falls about 40% below that peak.
/// - **Roughness.** A sawtooth's dense harmonics through two formant bands, the nasal
///   1.5 kHz and the bright 2.8 kHz, and beside each a second sawtooth [`SQUAWK_DETUNE`]
///   higher: the two beat at tens of hertz, amplitude modulation a throat makes.
/// - **Breath.** White noise through the same two formants and a hiss above them, so the
///   spectrum between the harmonics is filled rather than empty.
///
/// Every band, and every frequency a glide reaches, stays under 3.6 kHz: 0.45 of the lowest
/// supported rate.
fn squawk(variation: f32, envelope: Envelope) -> Vec<Layer> {
    let hz = 620.0 + variation * 160.0;
    let voice = |detune: f32, gain, formant, q| Layer {
        exciter: Exciter::Glide(Glide {
            wave: Wave::Saw,
            from: hz * detune,
            to: hz * detune * SQUAWK_FALL,
            seconds: SQUAWK_SECONDS,
            curve: Curve::Exponential,
            vibrato: Vibrato {
                hz: 0.5 / SQUAWK_SECONDS,
                depth: SQUAWK_ARCH,
                onset: 0.0,
            },
        }),
        gain,
        envelope,
        gate: None,
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz: formant,
            q,
        }),
    };
    let breath = |gain, kind, formant, q| Layer {
        envelope,
        ..noise(Noise::White, gain, kind, formant, q)
    };
    vec![
        voice(1.0, 0.26, 1500.0, 1.0),
        voice(SQUAWK_DETUNE, 0.19, 1500.0, 1.0),
        voice(1.0, 0.17, 2800.0, 1.4),
        voice(SQUAWK_DETUNE, 0.12, 2800.0, 1.4),
        breath(0.15, FilterKind::Band, 1500.0, 1.5),
        breath(0.15, FilterKind::Band, 2800.0, 2.0),
        breath(0.06, FilterKind::Band, 3400.0, 1.2),
    ]
}

/// The numbers one raptor's cry differs from another's by.
///
/// **The construction is [`scream`] and is shared; a species is this row of figures.** That is
/// the shape the issue behind the eagle asked for in as many words — the eagle and the condor
/// "should share that construction and differ in their numbers, not in their kind" — and it is
/// also the only way the two can be told apart by a test: a difference that lives in a number
/// can be asserted against, where a second hand-written copy of the same seven layers can only
/// be read.
struct Scream {
    /// The pitch the fall starts from at variation zero, and how far the seed lifts it.
    hz: f32,
    spread: f32,
    /// Where the falling line ends, as a fraction of where it starts.
    fall: f32,
    /// How far above the voice its rough twin sits. The two beat at `(detune - 1) * hz`, which
    /// is amplitude modulation a throat makes rather than a second note.
    detune: f32,
    /// The two formant bands the sawtooth is heard through, as `(hertz, q)`.
    formants: [(f32, f32); 2],
    /// A third band only the breath fills, above both formants: the air in the cry.
    hiss: (f32, f32),
    /// How loud the loudest voiced layer is, and how loud each of the two formant breaths is.
    /// **A hoarser bird is more air than pitch**, so the condor's two numbers are the eagle's
    /// the other way round.
    voiced: f32,
    breath: f32,
    /// The rasp, as a tremble on the falling line: fast and shallow is a rough throat, where
    /// slow and deep would be a siren. Its depth is a fraction of the pitch reached, so it
    /// stays the same interval wide all the way down, and its rate stays inside the 40 Hz
    /// `Sound::validate` allows a vibrato — a bound `every_raptor_stays_under_the_lowest_nyquist_margin`
    /// reaches by baking each description rather than by restating the number.
    rasp: Vibrato,
    /// How long the fall takes. The glide holds its arrival afterwards, so this is the cry and
    /// not the bake.
    seconds: f32,
}

/// The snow's eagle: high, bright and harsh, a scream that falls by two fifths.
const EAGLE: Scream = Scream {
    hz: 980.0,
    spread: 180.0,
    fall: 0.6,
    detune: 1.035,
    formants: [(2000.0, 1.1), (3100.0, 1.5)],
    hiss: (3400.0, 1.2),
    voiced: 0.24,
    breath: 0.13,
    rasp: Vibrato {
        // The synthesiser bounds a vibrato at 40 Hz (`Sound::validate`), so this is near the
        // fastest tremble a glide may carry — which is what a rough throat is.
        hz: 38.0,
        depth: 0.045,
        onset: 0.05,
    },
    seconds: 0.55,
};

/// The sand's condor: an octave and a half below the eagle, its formants down with it, more
/// air than pitch, and a coarser and slower rasp. A vulture's cry is a hoarse rasp rather
/// than a raptor's whistle, which is the whole of why its numbers are not the eagle's
/// transposed.
const CONDOR: Scream = Scream {
    hz: 300.0,
    spread: 60.0,
    fall: 0.72,
    detune: 1.05,
    formants: [(750.0, 0.9), (1500.0, 1.2)],
    hiss: (2400.0, 1.0),
    voiced: 0.15,
    breath: 0.26,
    rasp: Vibrato {
        hz: 26.0,
        depth: 0.07,
        onset: 0.04,
    },
    seconds: 0.8,
};

/// One raptor's cry: harsh, broadband and descending, never a note.
///
/// Built from the same vocabulary as [`squawk`], and deliberately so — that function is this
/// repository's worked example of a voice that is textured rather than tonal, and the eagle is
/// the voice it was never applied to:
///
/// - **A falling contour.** Every voiced layer rides one exponential glide from the seed's
///   pitch down to [`Scream::fall`] of it, so the cry descends the whole way through. A
///   squawk's single half-cycle of vibrato arches its pitch instead; a scream does not arch,
///   it falls, so the vibrato here is the rasp and nothing else.
/// - **Roughness.** A sawtooth's dense harmonics through two formant bands, and beside each a
///   second sawtooth [`Scream::detune`] higher: the pair beats at tens of hertz.
/// - **Breath.** White noise through the same two formants and a hiss above them, so the
///   spectrum between the harmonics is filled rather than empty. This is where a hoarse bird
///   spends its level.
///
/// Every band, and every frequency a glide reaches, stays under 3.6 kHz — 0.45 of the lowest
/// supported rate — so both cries bake at an 8 kHz device rate. The widest reach is the
/// detuned twin at the top of its variation, lifted by the rasp's depth:
/// `(hz + spread) * detune * (1 + rasp.depth)`, which is 1254 Hz for the eagle and 404 Hz for
/// the condor. `every_raptor_stays_under_the_lowest_nyquist_margin` is what holds that.
fn scream(spec: &Scream, variation: f32, envelope: Envelope) -> Vec<Layer> {
    let hz = spec.hz + variation * spec.spread;
    let voice = |detune: f32, gain: f32, (formant, q): (f32, f32)| Layer {
        exciter: Exciter::Glide(Glide {
            wave: Wave::Saw,
            from: hz * detune,
            to: hz * detune * spec.fall,
            seconds: spec.seconds,
            curve: Curve::Exponential,
            vibrato: spec.rasp,
        }),
        gain,
        envelope,
        gate: None,
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz: formant,
            q,
        }),
    };
    let breath = |gain, (formant, q)| Layer {
        envelope,
        ..noise(Noise::White, gain, FilterKind::Band, formant, q)
    };
    let [low, high] = spec.formants;
    vec![
        voice(1.0, spec.voiced, low),
        voice(spec.detune, spec.voiced * 0.73, low),
        voice(1.0, spec.voiced * 0.65, high),
        voice(spec.detune, spec.voiced * 0.46, high),
        breath(spec.breath, (low.0, 1.5)),
        breath(spec.breath, (high.0, 2.0)),
        breath(spec.breath * 0.42, spec.hiss),
    ]
}

/// The wolf's call, end to end. Its [`CallProfile`] length is named here because the howl's
/// one pitch line is drawn across exactly that long: the two cannot drift apart.
pub(super) const HOWL_SECONDS: f32 = 3.8;

/// How far the howl's rough twin sits above the voice: at 310 to 365 Hz, 2% beats at 6 to 7 Hz,
/// the slow waver of a voice held near the top of its range rather than a second note.
const HOWL_DETUNE: f32 = 1.02;

/// The gesture one howl makes, from its seed.
///
/// Units matter here and two of these five are fractions while two are seconds, so each says
/// which it is. `onset` in particular is **seconds**, not a fraction of anything: `Vibrato`
/// documents it as "opens linearly from nothing over `onset` seconds" and bounds it at 60,
/// which is why a value over 1.0 is ordinary rather than out of contract.
struct Howl {
    /// The pitch the call opens on, in hertz.
    hz: f32,
    /// How far the arch lifts the pitch above its falling line, as a fraction of that line —
    /// this is the vibrato's `depth`, which the synthesiser bounds at 0.5.
    arch: f32,
    /// How long the arch takes from nothing back to nothing, in seconds.
    span: f32,
    /// How long the arch takes to open from nothing, in seconds.
    onset: f32,
    /// Where the falling line ends, as a fraction of where it starts.
    close: f32,
}

/// The seed spread across the gesture's parameters. Each one reads its own field of a single
/// multiplication rather than its own shift of the seed: the pins exercise small literal seeds
/// whose high bytes are all zero, and a bare shift would hand several of them one gesture.
///
/// `no_two_howls_make_the_same_gesture` is where that claim is held to account, and it took the
/// review of #1200 to make it so: the test ran scrambled seeds alone, which a bare shift spreads
/// perfectly well, so nothing there could have failed on a revert of this function.
fn spread(seed: u64) -> u64 {
    (seed ^ 0x9e37_79b9_7f4a_7c15).wrapping_mul(0xd134_2543_de82_ef95)
}

/// A howl's gesture. The rise differs per seed in size (`arch`) and in how it opens (`onset`);
/// the fall differs in size (`arch` and `close` together) and in timing (`span`, which places
/// the top a little before half of it).
///
/// `span` is deliberately shorter than [`HOWL_SECONDS`]: the arch is back at nothing around
/// three seconds in and slightly under it by the end, so the last second of the call is an
/// audible fall rather than a fall crammed into the release. Measured across the seeds, the
/// pitch tops out between 1.1 and 1.6 s and closes between half and three quarters of where it
/// opened.
fn howl_gesture(seed: u64) -> Howl {
    let spread = spread(seed);
    Howl {
        hz: 310.0 + (seed % 101) as f32 / 100.0 * 55.0,
        arch: 0.22 + ((spread >> 16) % 17) as f32 / 100.0,
        span: 2.60 + ((spread >> 28) % 101) as f32 / 100.0,
        onset: 0.50 + ((spread >> 40) % 56) as f32 / 100.0,
        close: 0.80 + ((spread >> 52) % 11) as f32 / 100.0,
    }
}

/// The one pitch track every voiced layer of the howl follows, at a harmonic of it.
///
/// A line falls slowly from `hz` to `close` of it across the whole call, and a single
/// half-cycle of vibrato `span` long arches the pitch `arch` above that line and away again.
/// The arch is flat across its top, which is the held note in the middle of a howl; the falling
/// line underneath it is what makes the close lower than the opening rather than equal to it,
/// and `span` ending before the call does is what puts the fall inside the call.
///
/// Every harmonic has the same curve and the same *fractional* arch, so the partials stay
/// locked together as one voice instead of beating against each other.
fn howl_pitch(howl: &Howl, harmonic: f32, wave: Wave) -> Glide {
    Glide {
        wave,
        from: howl.hz * harmonic,
        to: howl.hz * howl.close * harmonic,
        seconds: HOWL_SECONDS,
        curve: Curve::Exponential,
        vibrato: Vibrato {
            hz: 0.5 / howl.span,
            depth: howl.arch,
            onset: howl.onset,
        },
    }
}

/// One howl of a wolf: a voice calling across a valley, not a note held for four seconds.
///
/// - **A pitch contour.** Every voiced layer rides one [`howl_pitch`]: the pitch rises about a
///   sixth into the call, holds within a few percent of its top for a second or so, and falls
///   away a third below that top. Both halves of the gesture vary with the seed.
/// - **Formants, not a harmonic series.** Three bands stand for a throat — the chest at 520 Hz
///   that carries the fundamental, the vowel at 1150 Hz that gives it a mouth, and a 2350 Hz
///   edge that opens late — and the waves under them are triangles and saws whose dense
///   harmonics the bands pick out, so the colour moves as the pitch walks across them.
/// - **Roughness and breath.** A twin [`HOWL_DETUNE`] above the chest voice beats with it at a
///   few hertz, and noise through the same three bands over a low wash fills the spectrum
///   between the harmonics rather than leaving it empty.
///
/// Every band, and every frequency a glide reaches with its arch, stays under 3.6 kHz: 0.45 of
/// the lowest supported rate, so the description bakes at an 8 kHz device.
fn howl(seed: u64, envelope: Envelope) -> Vec<Layer> {
    let howl = howl_gesture(seed);
    // Each band opens later and settles further into the call than the one below it, so the
    // howl brightens as it reaches its top instead of arriving whole.
    let voice = |harmonic, wave, gain, formant, q, attack, decay, sustain| Layer {
        exciter: Exciter::Glide(howl_pitch(&howl, harmonic, wave)),
        gain,
        envelope: Envelope {
            attack,
            decay,
            sustain,
            ..envelope
        },
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz: formant,
            q,
        }),
        gate: None,
    };
    let breath = |gain, kind, formant, q, attack| Layer {
        envelope: Envelope {
            attack,
            decay: 1.6,
            sustain: 0.5,
            ..envelope
        },
        ..noise(Noise::White, gain, kind, formant, q)
    };
    vec![
        // The chest, and the waver of a voice held there.
        voice(1.0, Wave::Triangle, 0.52, 520.0, 0.9, 0.8, 1.9, 0.42),
        voice(
            HOWL_DETUNE,
            Wave::Triangle,
            0.19,
            520.0,
            0.9,
            1.0,
            1.9,
            0.42,
        ),
        // The vowel: a saw read through a narrower band, and a third partial to seat it.
        voice(1.0, Wave::Saw, 0.26, 1150.0, 1.2, 1.1, 1.7, 0.45),
        voice(3.0, Wave::Sine, 0.14, 1150.0, 1.2, 1.4, 1.6, 0.45),
        // The edge, last to arrive and first to go.
        voice(1.0, Wave::Saw, 0.10, 2350.0, 2.0, 1.7, 1.4, 0.30),
        // The air the voice is made of. The three bands are wide (a low q) and a fourth layer
        // is low-passed rather than banded, because what fills the spectrum between the
        // harmonics has to cover the gaps between the formants too: three narrow bands leave
        // 1.6 to 2.1 kHz and everything above 2.6 kHz empty, and empty bins are what a
        // flatness measurement reads as a note.
        breath(0.15, FilterKind::Band, 520.0, 0.9, 0.7),
        breath(0.14, FilterKind::Band, 1150.0, 1.0, 1.0),
        breath(0.07, FilterKind::Band, 2350.0, 1.0, 1.5),
        breath(0.06, FilterKind::Low, 3400.0, 0.7, 1.2),
        // The low wash the call sits on.
        Layer {
            envelope: Envelope {
                attack: 0.9,
                decay: 2.0,
                sustain: 0.45,
                ..envelope
            },
            ..noise(Noise::Brown, 0.20, FilterKind::Low, 620.0, 0.7)
        },
    ]
}

/// The slowest and the fastest a rattle's train of clicks runs, in clicks a second. A real
/// rattle is dense; every wind-up starts and ends inside this band, and both ends stay far
/// under the gate's own bound of a twentieth of the sample rate — 400 at the 8 kHz a device
/// may open at.
const RATTLE_SLOWEST: f32 = 42.0;
const RATTLE_FASTEST: f32 = 88.0;

/// How long one click lasts, as a fraction of its own period: about four milliseconds at the
/// slowest rate and two at the fastest, so every opening is an impact and four fifths of every
/// period is exact silence.
const RATTLE_DUTY: f32 = 0.18;

/// The three bands one click is coloured by — a dry body, the buzz that carries, and the dust
/// at the top — as `(gain, hertz, q)`. White noise through all three and no oscillator
/// anywhere, because a rattle has no pitch; the highest band stays under 3.6 kHz, 0.45 of the
/// lowest supported rate, so the whole description bakes at 8 kHz.
const RATTLE_BANDS: [(f32, f32, f32); 3] =
    [(0.5, 1150.0, 0.9), (0.62, 2050.0, 1.1), (0.34, 3150.0, 1.3)];

/// One rattlesnake's rattle: a train of dry clicks that winds up, holds, and falls away.
///
/// - **A train, not a tone.** Every layer is white noise through one of [`RATTLE_BANDS`],
///   struck open and shut by a single shared [`Gate`]. The three layers carry the same gate
///   description, so their openings coincide exactly and each one is a single click with three
///   colours rather than three clicks a listener could count apart.
/// - **A wind-up no two seeds share.** The gate's rate climbs from `from` to `to` over
///   `seconds`, and all three come off different slices of the seed: the rate it starts at, the
///   rate it reaches, and how long it takes to get there are independent, so the span and its
///   duration are not one number wearing two hats.
/// - **Loudness that moves with it.** The call's envelope rises over its attack and settles to
///   its sustain, and the bake's own release fades the end of it away.
///
/// The gate is what makes this describable at all: [`Sound::bake_at`] strikes a description
/// again at each onset, and a one-second rattle needs tens of strikes where that is bounded by
/// sixteen.
fn rattle(variation: f32, seed: u64, envelope: Envelope) -> Vec<Layer> {
    let gate = Gate {
        from: RATTLE_SLOWEST + variation * 13.0,
        to: RATTLE_FASTEST - ((seed >> 24) % 15) as f32,
        seconds: 0.26 + ((seed >> 40) % 23) as f32 / 100.0,
        // Equal proportion in equal time: a rattle accelerates the way it is wound up, rather
        // than by the same number of clicks a second every second.
        curve: Curve::Exponential,
        duty: RATTLE_DUTY,
    };
    RATTLE_BANDS
        .into_iter()
        .map(|(gain, hz, q)| Layer {
            envelope,
            gate: Some(gate),
            ..noise(Noise::White, gain, FilterKind::Band, hz, q)
        })
        .collect()
}

impl Call {
    pub(super) fn profile(self) -> CallProfile {
        let (interval, radius, height, seconds, range) = match self {
            Self::Rattlesnake => ([12.0, 31.0], 5.0, -1.3, 0.8, 24.0),
            // The 0.55 s fall plus the 0.14 s release ends inside the baked 0.74 s, leaving
            // 0.05 s of arrival pitch held open before the close. **It was 0.65 s, which did
            // not fit**: the release began at 0.51 s, four hundredths *before* the glide had
            // finished falling, so the cry was at 44% of its peak and dropping at the moment
            // it reached its lowest pitch — the envelope-shaped fall this issue set out to
            // replace, reintroduced by a length rather than by a description.
            // `a_raptor_cry_finishes_falling_before_its_release_begins` is what now holds it.
            Self::Eagle => ([9.0, 24.0], 18.0, 35.0, 0.74, 96.0),
            // High over the sand, in the band `BIRDS[VULTURE]` circles in (25 to 45 blocks),
            // and **sparser than the eagle**: a bird that calls seldom, as the issue asks. The
            // 0.8 s fall plus the 0.1 s release ends inside the baked 0.95 s, the same 0.05 s
            // of held arrival.
            Self::Condor => ([26.0, 58.0], 24.0, 30.0, 0.95, 96.0),
            Self::Wolf => ([35.0, 79.0], 26.0, 0.0, HOWL_SECONDS, 96.0),
            // In the grass a few blocks off. The longest call, three syllables at the slowest
            // spacing, ends at 2 * 0.19 + 0.06 = 0.44 s, inside the baked 0.45 s.
            Self::Cricket => ([6.0, 16.0], 4.0, -1.2, 0.45, 24.0),
            // In a tree seven blocks off and five up, where the day lane always placed it, and
            // a few calls a minute rather than one every second (#1176). Two squawks at the
            // slowest spacing end at 0.52 + 0.3 = 0.82 s, inside the baked 0.85 s.
            Self::Parrot => ([8.0, 22.0], 7.0, 5.0, 0.85, 32.0),
        };
        CallProfile {
            interval,
            radius,
            height,
            seconds,
            range,
        }
    }

    /// One call rendered at the device's rate, from its seed. Every call is its description
    /// baked once for its profile's length — except the cricket and the macaw, whose
    /// descriptions are one syllable, struck at each of [`syllable_onsets`] or
    /// [`squawk_onsets`] with silence between.
    pub(super) fn bake(self, seed: u64, rate: u32) -> Result<Baked, synth::Error> {
        let seconds = self.profile().seconds;
        match self {
            Self::Cricket => self.description(seed).bake_at(
                &syllable_onsets(seed),
                SYLLABLE_SECONDS,
                seconds,
                rate,
                seed,
            ),
            Self::Parrot => self.description(seed).bake_at(
                &squawk_onsets(seed),
                SQUAWK_SECONDS,
                seconds,
                rate,
                seed,
            ),
            _ => self.description(seed).bake(seconds, rate, seed),
        }
    }

    pub(super) fn description(self, seed: u64) -> Sound {
        // **No bare `tone` closure lives here any more, and that is the shape of the change
        // rather than a tidy-up.** It built a clean sine partial, and every voice that reached
        // for one was a voice this repository has since found to whistle: the rattlesnake
        // (#1184), the wolf (#1185), and now the eagle (#1186). Each is a named construction —
        // `rattle`, `howl`, `scream`, `squawk` — and the last caller went with the eagle.
        let variation = (seed % 101) as f32 / 100.0;
        let (attack, decay, sustain, release) = match self {
            // A rattle is wound up rather than started: the level climbs over a tenth of a
            // second, settles high, and the bake's release takes the last quarter away.
            Self::Rattlesnake => (0.1, 0.22, 0.82, 0.25),
            // A scream: struck hard, held open while the pitch falls, closed quickly. The old
            // eagle decayed to a zero sustain over 0.4 s, which made the fall an envelope
            // rather than a pitch.
            Self::Eagle => (0.012, 0.1, 0.62, 0.14),
            // The same shape with a softer onset and a longer close: a vulture's cry is
            // breathed rather than struck.
            Self::Condor => (0.03, 0.14, 0.66, 0.1),
            Self::Wolf => (0.8, 1.8, 0.35, 1.2),
            // One syllable: a quick scrape that settles and is cut off before the next.
            Self::Cricket => (0.005, 0.025, 0.85, 0.02),
            // One squawk: a hard but unclicked onset, a harsh held middle, a quick close.
            Self::Parrot => (0.012, 0.08, 0.7, 0.06),
        };
        let envelope = Envelope {
            attack,
            decay,
            sustain,
            release,
        };
        let layers = match self {
            Self::Rattlesnake => rattle(variation, seed, envelope),
            // Two raptors, one construction, two rows of numbers. What was here for the eagle
            // was `tone(hz, ..)` at 2.1 kHz and a 1.35× partial — two clean sines, which
            // whistle; that is the finding `a_squawk_is_harsh_and_broadband_and_not_a_note`
            // was written from, and the eagle is the voice it had never been applied to.
            Self::Eagle => scream(&EAGLE, variation, envelope),
            Self::Condor => scream(&CONDOR, variation, envelope),
            // A voice with a pitch contour, not a harmonic stack whose colour opens: see
            // [`howl`]. The gesture is read from the whole seed rather than from `variation`.
            Self::Wolf => howl(seed, envelope),
            // The cricket voice from before #1145, which was right in timbre and wrong only in
            // never stopping: white noise through a narrow band (q 8) near 3.2 kHz, a scraped
            // shimmer rather than #1145's pure whistle. Four such bands side by side, not one:
            // a syllable is sixty milliseconds rather than a bed, and uncorrelated bands add
            // level as well as width. The top band stays under 8 kHz's 3.6 kHz bound.
            Self::Cricket => {
                let hz = 3000.0 + variation * 100.0;
                [0.0, 150.0, 300.0, 450.0]
                    .into_iter()
                    .map(|offset| Layer {
                        envelope,
                        ..noise(Noise::White, 1.0, FilterKind::Band, hz + offset, 8.0)
                    })
                    .collect()
            }
            Self::Parrot => squawk(variation, envelope),
        };
        Sound { layers }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::spatial;
    fn stream(bed: Bed, rate: u32, seed: u64) -> Vec<f32> {
        let mut stream = bed.description().continuous(rate, seed).unwrap();
        let mut output = vec![0.0; rate as usize * 2];
        assert_eq!(stream.render(&mut output), output.len());
        output
    }
    #[test]
    fn every_bed_is_finite_audible_and_fresh_at_supported_rates() {
        for bed in super::super::BEDS {
            for rate in [8000, 44100, 48000, 192000] {
                let samples = stream(bed, rate, 17);
                assert!(samples.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
                let second = &samples[rate as usize..];
                assert!(second.iter().any(|v| v.abs() > 0.001));
                assert_ne!(&second[..second.len() / 2], &second[second.len() / 2..]);
            }
        }
    }
    #[test]
    fn weather_beds_have_distinct_spectra() {
        let bright = |bed| {
            let v = stream(bed, 48000, 11);
            v.windows(2).map(|p| (p[1] - p[0]).powi(2)).sum::<f32>()
                / v.iter().map(|v| v * v).sum::<f32>()
        };
        assert!(bright(Bed::Rain) > bright(Bed::DrivingRain) * 3.0);
        assert!(bright(Bed::Rain) > bright(Bed::Snowfall) * 10.0);
    }
    #[test]
    fn descriptions_are_seeded_and_calls_have_silent_edges() {
        assert_eq!(stream(Bed::Rain, 8000, 2), stream(Bed::Rain, 8000, 2));
        assert_ne!(stream(Bed::Rain, 8000, 2), stream(Bed::Rain, 8000, 3));
        for rate in [8000, 48000, 192000] {
            let call = Call::Parrot.bake(7, rate).unwrap();
            assert_eq!(call.samples()[0], 0.0);
            assert_eq!(*call.samples().last().unwrap(), 0.0);
            assert!(call.samples().iter().any(|v| v.abs() > 0.01));
            assert_ne!(
                call.samples(),
                Call::Parrot.bake(11, rate).unwrap().samples()
            );
        }
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |peak, v| v.abs().max(peak))
    }

    fn scramble(seed: u64) -> u64 {
        super::super::controller::scramble(seed)
    }

    /// The syllables actually rendered, as `(first, last)` sounding sample: runs of sound
    /// separated by at least 20 ms of exact silence. Band-passed noise crosses zero, but never
    /// holds it for twenty milliseconds.
    fn rendered_syllables(samples: &[f32], rate: u32) -> Vec<(usize, usize)> {
        let gap = rate as usize / 50;
        let mut found: Vec<(usize, usize)> = Vec::new();
        for (index, _) in samples.iter().enumerate().filter(|(_, v)| **v != 0.0) {
            match found.last_mut() {
                Some((_, last)) if index - *last <= gap => *last = index,
                _ => found.push((index, index)),
            }
        }
        found
    }

    /// #1161: an occasional "cri-cri". A call is two or three short syllables with real
    /// silence between them — not #1145's one to three faint pulses, and not a trill.
    #[test]
    fn a_cricket_call_is_two_or_three_syllables_with_silence_between() {
        let mut seen = [false; 2];
        for seed in (0..60u64).map(scramble) {
            let expected = syllables(seed);
            seen[expected - 2] = true;
            for rate in [8000, 48000] {
                let call = Call::Cricket.bake(seed, rate).unwrap();
                let found = rendered_syllables(call.samples(), rate);
                assert_eq!(found.len(), expected, "seed {seed} at {rate}: {found:?}");
                for (first, last) in &found {
                    let seconds = (last - first) as f32 / rate as f32;
                    assert!(
                        seconds > 0.03 && seconds <= SYLLABLE_SECONDS,
                        "seed {seed} at {rate}: a {seconds} s syllable"
                    );
                }
                for pair in found.windows(2) {
                    let silence = (pair[1].0 - pair[0].1) as f32 / rate as f32;
                    assert!(
                        silence >= 0.08,
                        "seed {seed} at {rate}: {silence} s between syllables"
                    );
                }
            }
        }
        assert_eq!(seen, [true; 2], "both syllable counts occur");
    }

    /// The share of a sound's energy within 60 Hz of its loudest frequency between 1 and 4 kHz.
    /// Goertzel's recurrence per DFT bin, so no crate and any length. A pure tone keeps nearly
    /// all of its energy on one frequency however it is enveloped; a scraped band spreads it.
    fn tonal_share(samples: &[f32], rate: u32) -> f32 {
        let n = samples.len();
        let bin = |hz: f32| (hz * n as f32 / rate as f32).round() as usize;
        let power: Vec<(usize, f64)> = (bin(1000.0)..bin(4000.0).min(n / 2))
            .map(|k| {
                let coefficient = 2.0 * (std::f64::consts::TAU * k as f64 / n as f64).cos();
                let (mut s1, mut s2) = (0.0f64, 0.0f64);
                for &x in samples {
                    let s0 = f64::from(x) + coefficient * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                (k, s1 * s1 + s2 * s2 - coefficient * s1 * s2)
            })
            .collect();
        let loudest = power
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(0, |(k, _)| *k);
        let near = power
            .iter()
            .filter(|(k, _)| k.abs_diff(loudest) <= bin(60.0))
            .map(|(_, p)| p)
            .sum::<f64>();
        // Parseval: the positive half of the spectrum holds n / 2 times the sample energy.
        let energy: f64 = samples.iter().map(|x| f64::from(*x).powi(2)).sum();
        (near / (energy * n as f64 / 2.0)) as f32
    }

    /// The owner's report on #1161: the cricket had become "just a whistle", a pure tone.
    /// The voice from before #1145 was a band of noise, and that is what a call is again. The
    /// negative control is the point: the same syllables voiced as a sine — the whistle — fail
    /// the same measurement, so the floor separates the two rather than passing everything.
    #[test]
    fn a_cricket_chirp_is_a_scraped_band_and_not_a_whistle() {
        let profile = Call::Cricket.profile();
        for seed in (0..20u64).map(scramble) {
            let call = Call::Cricket.bake(seed, 8000).unwrap();
            let share = tonal_share(call.samples(), 8000);
            assert!(
                share < 0.4,
                "seed {seed}: {share} of the energy on one frequency"
            );
        }
        let seed = scramble(7);
        let whistle = Sound {
            layers: vec![Layer {
                exciter: Exciter::Oscillator {
                    wave: Wave::Sine,
                    hz: 3200.0,
                },
                gain: 0.5,
                envelope: Call::Cricket.description(seed).layers[0].envelope,
                gate: None,
                filter: None,
            }],
        }
        .bake_at(
            &syllable_onsets(seed),
            SYLLABLE_SECONDS,
            profile.seconds,
            8000,
            seed,
        )
        .unwrap();
        let share = tonal_share(whistle.samples(), 8000);
        assert!(share > 0.8, "a whistle measured {share}");
    }

    /// Heard where the lane places it — `radius` out and `height` down, faded by the same
    /// `spatial::attenuation` every placed sound is — before the Ambience bus, at the 48 kHz a
    /// device usually runs. #1145's call peaked near 0.12 here. The RMS is over the syllables,
    /// because the silence between them is the design rather than a lack of level.
    #[test]
    fn a_cricket_call_placed_at_its_radius_is_clearly_heard() {
        let profile = Call::Cricket.profile();
        let gain = spatial::attenuation(profile.radius.hypot(profile.height), profile.range);
        for seed in (0..60u64).map(scramble) {
            let call = Call::Cricket.bake(seed, 48000).unwrap();
            let heard: Vec<f32> = call.samples().iter().map(|v| v * gain).collect();
            let voiced: Vec<f32> = rendered_syllables(&heard, 48000)
                .into_iter()
                .flat_map(|(first, last)| heard[first..=last].to_vec())
                .collect();
            let rms = (voiced.iter().map(|v| v * v).sum::<f32>() / voiced.len() as f32).sqrt();
            assert!(
                peak(&heard) >= 0.2 && rms >= 0.05,
                "seed {seed}: peak {}, rms {rms}",
                peak(&heard)
            );
        }
    }

    /// The call #1176 replaced, verbatim: two sine partials at `hz` and `hz * 1.7` over a
    /// band of noise. The owner's recording matched it on every axis.
    fn old_parrot(seed: u64) -> Sound {
        let hz = 1150.0 + (seed % 401) as f32;
        let envelope = Envelope {
            attack: 0.015,
            decay: 0.11,
            sustain: 0.0,
            release: 0.04,
        };
        Sound {
            layers: vec![
                Layer {
                    exciter: Exciter::Oscillator {
                        wave: Wave::Sine,
                        hz,
                    },
                    gain: 0.16,
                    envelope,
                    gate: None,
                    filter: None,
                },
                Layer {
                    exciter: Exciter::Oscillator {
                        wave: Wave::Sine,
                        hz: hz * 1.7,
                    },
                    gain: 0.07,
                    envelope,
                    gate: None,
                    filter: None,
                },
                Layer {
                    envelope: Envelope {
                        attack: 0.01,
                        decay: 0.14,
                        sustain: 0.0,
                        release: 0.04,
                    },
                    ..noise(Noise::White, 0.11, FilterKind::Band, 1600.0, 1.5)
                },
            ],
        }
    }

    /// The new squawk with its voice made clean: every voiced layer the same glide as a sine,
    /// and no breath. The contour survives; the rasp and the air do not.
    fn clean_squawk(seed: u64) -> Sound {
        Sound {
            layers: Call::Parrot
                .description(seed)
                .layers
                .into_iter()
                .filter_map(|layer| match layer.exciter {
                    Exciter::Glide(glide) => Some(Layer {
                        exciter: Exciter::Glide(Glide {
                            wave: Wave::Sine,
                            ..glide
                        }),
                        filter: None,
                        ..layer
                    }),
                    _ => None,
                })
                .collect(),
        }
    }

    /// Energy per DFT bin from `low` to `high` hertz, one bin per `1 / duration` hertz, by
    /// Goertzel's recurrence as in [`tonal_share`].
    fn band_power(samples: &[f32], rate: u32, low: f32, high: f32) -> Vec<f64> {
        let n = samples.len();
        let bin = |hz: f32| (hz * n as f32 / rate as f32).round() as usize;
        (bin(low)..bin(high).min(n / 2))
            .map(|k| {
                let coefficient = 2.0 * (std::f64::consts::TAU * k as f64 / n as f64).cos();
                let (mut s1, mut s2) = (0.0f64, 0.0f64);
                for &x in samples {
                    let s0 = f64::from(x) + coefficient * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                s1 * s1 + s2 * s2 - coefficient * s1 * s2
            })
            .collect()
    }

    /// Spectral flatness from 400 Hz to 3.6 kHz: the geometric mean of the power spectrum over
    /// its arithmetic mean. One for a perfectly flat spectrum, about 0.56 for one periodogram of
    /// white noise, and near zero for clean partials, whose energy is on a few bins and whose
    /// other bins are empty. A voice made rough and breathy fills the bins between harmonics.
    fn flatness(samples: &[f32], rate: u32) -> f64 {
        let power = band_power(samples, rate, 400.0, 3600.0);
        let floor = power.iter().sum::<f64>() / power.len() as f64 * 1e-12;
        let log = power.iter().map(|p| (p + floor).ln()).sum::<f64>() / power.len() as f64;
        log.exp() / (power.iter().sum::<f64>() / power.len() as f64)
    }

    /// The squawk's voiced layers alone, one squawk baked at 8 kHz: the pitch without the
    /// breath, so a tracker reads the voice rather than the noise around it.
    fn voiced_squawk(seed: u64) -> Vec<f32> {
        Sound {
            layers: Call::Parrot
                .description(seed)
                .layers
                .into_iter()
                .filter(|layer| matches!(layer.exciter, Exciter::Glide(_)))
                .collect(),
        }
        .bake(SQUAWK_SECONDS, 8000, seed)
        .unwrap()
        .samples()
        .to_vec()
    }

    /// The dominant frequency between `low` and `high` hertz every ten milliseconds, from a
    /// Hann-windowed thirty-millisecond window, read on a 5 Hz grid. Windows under a twentieth
    /// of the loudest one's energy are skipped: the onset and the release have no pitch worth
    /// reading.
    fn dominant_track(samples: &[f32], rate: u32, low: f32, high: f32) -> Vec<(f32, f32)> {
        let window = rate as usize * 3 / 100;
        let hop = rate as usize / 100;
        let hann: Vec<f32> = (0..window)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / window as f32).cos())
            .collect();
        let frames: Vec<(usize, Vec<f32>)> = (0..samples.len() - window)
            .step_by(hop)
            .map(|start| {
                let frame = samples[start..start + window]
                    .iter()
                    .zip(&hann)
                    .map(|(x, w)| x * w)
                    .collect();
                (start, frame)
            })
            .collect();
        let energy = |frame: &[f32]| frame.iter().map(|x| x * x).sum::<f32>();
        let loudest = frames.iter().map(|(_, f)| energy(f)).fold(0.0, f32::max);
        frames
            .into_iter()
            .filter(|(_, frame)| energy(frame) >= loudest * 0.05)
            .map(|(start, frame)| {
                let power = |hz: f32| {
                    let coefficient =
                        2.0 * (std::f64::consts::TAU * f64::from(hz / rate as f32)).cos();
                    let (mut s1, mut s2) = (0.0f64, 0.0f64);
                    for &x in &frame {
                        let s0 = f64::from(x) + coefficient * s1 - s2;
                        s2 = s1;
                        s1 = s0;
                    }
                    s1 * s1 + s2 * s2 - coefficient * s1 * s2
                };
                let hz = (0..)
                    .map(|step| low + step as f32 * 5.0)
                    .take_while(|hz| *hz <= high)
                    .max_by(|a, b| power(*a).total_cmp(&power(*b)))
                    .unwrap_or(low);
                ((start + window / 2) as f32 / rate as f32, hz)
            })
            .collect()
    }

    /// The pitch a squawk's glide starts from, read back from its description.
    fn squawk_pitch(seed: u64) -> f32 {
        match Call::Parrot.description(seed).layers[0].exciter {
            Exciter::Glide(glide) => glide.from,
            other => panic!("the squawk's first layer is not voiced: {other:?}"),
        }
    }

    /// #1176: the owner's rule is that a sound is realistic, never a note, and the day call was
    /// two sine partials. A squawk spreads its energy across the whole band a macaw fills, where
    /// a note keeps it on a few frequencies with nothing between them. The negative controls are
    /// the point: the call it replaced, a strict sine pair, and this very squawk voiced as clean
    /// sines along the same contour all fail the measurement the squawk passes.
    #[test]
    fn a_squawk_is_harsh_and_broadband_and_not_a_note() {
        let profile = Call::Parrot.profile();
        for seed in (0..20u64).map(scramble) {
            let call = Call::Parrot.bake(seed, 8000).unwrap();
            let flat = flatness(call.samples(), 8000);
            let tonal = tonal_share(call.samples(), 8000);
            assert!(
                flat > 0.15 && tonal < 0.4,
                "seed {seed}: flatness {flat}, {tonal} of the energy on one frequency"
            );
            let old = old_parrot(seed).bake(0.3, 8000, seed).unwrap();
            let pair = Sound {
                layers: old_parrot(seed).layers[..2].to_vec(),
            }
            .bake(0.3, 8000, seed)
            .unwrap();
            let clean = clean_squawk(seed)
                .bake_at(
                    &squawk_onsets(seed),
                    SQUAWK_SECONDS,
                    profile.seconds,
                    8000,
                    seed,
                )
                .unwrap();
            for (name, note) in [
                ("the old call", old),
                ("a sine pair", pair),
                ("a clean squawk", clean),
            ] {
                let flat = flatness(note.samples(), 8000);
                assert!(flat < 0.05, "seed {seed}: {name} measured flatness {flat}");
            }
        }
    }

    /// A squawk's pitch moves: it rises into the call and falls out of it, and is never steady.
    /// Read on the voiced layers alone, between 0.6 and 1.3 times the pitch the glide starts
    /// from — a band the fundamental stays inside for the whole squawk and its second harmonic
    /// never enters.
    #[test]
    fn a_squawk_rises_into_the_call_and_falls_out_of_it() {
        for seed in (0..12u64).map(scramble) {
            let hz = squawk_pitch(seed);
            let track = dominant_track(&voiced_squawk(seed), 8000, hz * 0.6, hz * 1.3);
            assert!(
                track.len() >= 20,
                "seed {seed}: {} voiced windows",
                track.len()
            );
            let (time, top) = track
                .iter()
                .copied()
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            let (first, last) = (track[0].1, track[track.len() - 1].1);
            assert!(
                top >= first * 1.08,
                "seed {seed}: rises from {first} to only {top} Hz"
            );
            assert!(
                top >= last * 1.25,
                "seed {seed}: falls from {top} to only {last} Hz"
            );
            assert!(
                (0.05..=0.2).contains(&time),
                "seed {seed}: peaks at {time} s"
            );
        }
    }

    /// A call is one squawk or two, from its seed, with real silence between two.
    #[test]
    fn a_macaw_call_is_one_or_two_squawks_with_silence_between() {
        let mut seen = [false; 2];
        for seed in (0..40u64).map(scramble) {
            let expected = squawks(seed);
            seen[expected - 1] = true;
            for rate in [8000, 48000] {
                let call = Call::Parrot.bake(seed, rate).unwrap();
                let found = rendered_syllables(call.samples(), rate);
                assert_eq!(found.len(), expected, "seed {seed} at {rate}: {found:?}");
                for (first, last) in &found {
                    let seconds = (last - first) as f32 / rate as f32;
                    assert!(
                        seconds > 0.2 && seconds <= SQUAWK_SECONDS,
                        "seed {seed} at {rate}: a {seconds} s squawk"
                    );
                }
                for pair in found.windows(2) {
                    let silence = (pair[1].0 - pair[0].1) as f32 / rate as f32;
                    assert!(
                        silence >= 0.1,
                        "seed {seed} at {rate}: {silence} s between squawks"
                    );
                }
            }
        }
        assert_eq!(seen, [true; 2], "both squawk counts occur");
    }

    /// The eagle #1186 replaced, verbatim: a sine at 2.1 kHz plus a 1.35× partial with a
    /// staggered attack, decaying to a zero sustain over four tenths of a second. Two clean
    /// partials, which is what whistles.
    fn old_eagle(seed: u64) -> Sound {
        let variation = (seed % 101) as f32 / 100.0;
        let hz = 2100.0 + variation * 300.0;
        let envelope = Envelope {
            attack: 0.015,
            decay: 0.4,
            sustain: 0.0,
            release: 0.1,
        };
        let tone = |hz, gain, envelope| Layer {
            exciter: Exciter::Oscillator {
                wave: Wave::Sine,
                hz,
            },
            gain,
            envelope,
            filter: None,
            gate: None,
        };
        Sound {
            layers: vec![
                tone(hz, 0.36, envelope),
                tone(
                    hz * 1.35,
                    0.12,
                    Envelope {
                        attack: 0.08,
                        ..envelope
                    },
                ),
            ],
        }
    }

    /// The rate every howl measurement below renders at. A rate the synthesiser accepts, chosen
    /// for the tracker rather than for a device: high enough that a 300 Hz period is fifty
    /// samples and a parabola through its neighbours resolves the contour to a fraction of a
    /// percent, low enough that an autocorrelation over three hundred and eighty windows of a
    /// 3.8 s call is cheap. The pins and the carry test cover the rates a device opens at.
    const HOWL_RATE: u32 = 16000;

    /// Every seed the howl measurements run: the three the pin table pins, read from
    /// `pins::SEEDS` itself rather than copied, and then eight scrambled ones.
    ///
    /// The pinned seeds are here because they are the only seeds a regression in [`spread`]
    /// could land on, and a suite that ran scrambled seeds alone could not see one. A
    /// scrambled seed has its high bytes set, so reading the gesture straight off `seed >> 28`
    /// and its neighbours distributes those perfectly well; the entire justification for mixing
    /// the seed first is the literal `0x0` and `0x10203`, whose high bytes are zero. Raised on
    /// the review of #1200, and it is this repository's recurring defect one layer out: a guard
    /// whose inputs cannot reach the thing it guards reads exactly like one whose inputs can.
    ///
    /// Pinned first, so `no_two_howls_make_the_same_gesture` can slice them back off the front.
    fn howl_seeds() -> impl Iterator<Item = u64> {
        super::super::pins::SEEDS
            .into_iter()
            .chain((0..8u64).map(scramble))
    }

    /// The pitch a voiced passage is at, every ten milliseconds, from a forty-millisecond
    /// window: the shortest lag between `low` and `high` hertz whose normalised autocorrelation
    /// comes within a tenth of the best, placed between samples by a parabola through its two
    /// neighbours. The shortest such lag rather than the best, so a period twice as long never
    /// reads as an octave's fall. Windows under a twentieth of the loudest one's energy are
    /// skipped: a howl's long attack and longer release have no pitch worth reading.
    ///
    /// Autocorrelation rather than [`dominant_track`]'s spectral peak, which is what the
    /// squawk uses: a howl's fundamental sits near 300 Hz, and no window short enough to follow
    /// the gesture resolves a 40 Hz move down there, while its period is fifty samples and
    /// reads exactly.
    ///
    /// **The whinny's tracker (#1160) is the same technique, and deliberately not the same
    /// numbers** — it fixes a 30 ms window at one rate over a 300–1800 Hz band with a 4% energy
    /// gate, where this one takes the rate and the band as arguments, opens the window to 40 ms
    /// and gates at 5%, because the register it reads is an octave and a half lower. Raised on
    /// the review of #1200 as a duplication that could drift: it can, and what that would cost
    /// is worth stating exactly. **It cannot invalidate anything measured here.** Both the howl
    /// and the [`old_wolf`] control go through *this* function, on the same band, in the same
    /// test — the comparison is internal to one copy, so the whinny's copy changing underneath
    /// it changes nothing. What duplication costs is a fix applied twice, and unifying the two
    /// would mean editing `client/src/player/mount_audio/`, which #1185 puts out of scope. No
    /// `TODO` with an invented issue number is left behind for it: a stand-in is exactly what
    /// this repository does not do, so the note is here and the follow-up is the owner's call.
    fn pitch_track(samples: &[f32], rate: u32, low: f32, high: f32) -> Vec<(f32, f32)> {
        let window = rate as usize * 4 / 100;
        let hop = rate as usize / 100;
        let shortest = (rate as f32 / high) as usize;
        let longest = (rate as f32 / low).ceil() as usize;
        let energy = |frame: &[f32]| frame.iter().map(|v| v * v).sum::<f32>();
        let starts: Vec<usize> = (0..samples.len().saturating_sub(window + longest + 2))
            .step_by(hop)
            .collect();
        let loudest = starts
            .iter()
            .map(|start| energy(&samples[*start..start + window]))
            .fold(0.0, f32::max);
        starts
            .into_iter()
            .filter_map(|start| {
                let frame = &samples[start..start + window];
                if energy(frame) < loudest * 0.05 {
                    return None;
                }
                let correlation = |lag: usize| {
                    let later = &samples[start + lag..start + lag + window];
                    let (mut cross, mut here, mut there) = (0.0f64, 0.0f64, 0.0f64);
                    for (x, y) in frame.iter().zip(later) {
                        let (x, y) = (f64::from(*x), f64::from(*y));
                        cross += x * y;
                        here += x * x;
                        there += y * y;
                    }
                    cross / (here * there).sqrt()
                };
                let r: Vec<f64> = (0..=longest + 1)
                    .map(|lag| {
                        if lag + 1 < shortest {
                            0.0
                        } else {
                            correlation(lag)
                        }
                    })
                    .collect();
                let best = r[shortest..=longest]
                    .iter()
                    .copied()
                    .fold(f64::MIN, f64::max);
                let lag = (shortest..=longest).find(|lag| {
                    r[*lag] >= best * 0.9 && r[*lag] >= r[lag - 1] && r[*lag] >= r[lag + 1]
                })?;
                let (before, at, after) = (r[lag - 1], r[lag], r[lag + 1]);
                let offset = 0.5 * (before - after) / (before - 2.0 * at + after);
                Some((
                    (start + window / 2) as f32 / rate as f32,
                    (f64::from(rate) / (lag as f64 + offset)) as f32,
                ))
            })
            .collect()
    }

    /// The gesture a pitch track makes: where it starts, its top and when it reaches it, where
    /// it ends, and how long it stays within 4% of that top. The hold is the span from the
    /// first such window to the last, which is the held middle of a call whose pitch rises once
    /// and falls once — and would overstate a track that wandered up to its top twice.
    fn contour(track: &[(f32, f32)]) -> (f32, f32, f32, f32, f32) {
        let (time, top) = track
            .iter()
            .copied()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let held: Vec<f32> = track
            .iter()
            .filter(|(_, hz)| *hz >= top * 0.96)
            .map(|(at, _)| *at)
            .collect();
        let hold = held[held.len() - 1] - held[0];
        (track[0].1, top, time, track[track.len() - 1].1, hold)
    }

    /// The howl's voiced layers alone, baked at [`HOWL_RATE`]: the pitch without the breath, so
    /// a tracker reads the voice rather than the noise around it — as [`voiced_squawk`] does.
    fn voiced_howl(seed: u64) -> Vec<f32> {
        Sound {
            layers: Call::Wolf
                .description(seed)
                .layers
                .into_iter()
                .filter(|layer| matches!(layer.exciter, Exciter::Glide(_)))
                .collect(),
        }
        .bake(HOWL_SECONDS, HOWL_RATE, seed)
        .unwrap()
        .samples()
        .to_vec()
    }

    /// The call #1185 replaced, verbatim: three sine partials at `hz`, twice it and 3.01 times
    /// it, the upper two fading in at 1.3 s and 1.8 s, over a soft band of breath at 650 Hz.
    /// The staggered attacks change the colour across the call, which was the intent; the pitch
    /// never moves, which is what was wrong with it.
    fn old_wolf(seed: u64) -> Sound {
        let variation = (seed % 101) as f32 / 100.0;
        let envelope = Envelope {
            attack: 0.8,
            decay: 1.8,
            sustain: 0.35,
            release: 1.2,
        };
        let tone = |hz, gain, envelope| Layer {
            exciter: Exciter::Oscillator {
                wave: Wave::Sine,
                hz,
            },
            gain,
            envelope,
            filter: None,
            gate: None,
        };
        let hz = 310.0 + variation * 55.0;
        Sound {
            layers: vec![
                tone(hz, 0.44, envelope),
                tone(
                    hz * 2.0,
                    0.22,
                    Envelope {
                        attack: 1.3,
                        ..envelope
                    },
                ),
                tone(
                    hz * 3.01,
                    0.08,
                    Envelope {
                        attack: 1.8,
                        ..envelope
                    },
                ),
                Layer {
                    envelope,
                    ..noise(Noise::White, 0.06, FilterKind::Band, 650.0, 1.0)
                },
            ],
        }
    }

    /// A cry with its voice made clean: every voiced layer the same glide as a sine, no
    /// filter, and no breath.
    ///
    /// **What survives is the whole gesture — the falling contour *and* its rasp**, since
    /// `..glide` keeps every field but `wave`, and [`Scream::rasp`] is that glide's `vibrato`.
    /// What is removed is the sawtooth's dense harmonics, the formant bands that shape them,
    /// and the breath beside them: the timbre, and nothing about the movement.
    ///
    /// **That is deliberate, and it is what makes this a control worth having.** A control
    /// that also dropped the vibrato would differ from the real cry in two ways at once, and
    /// passing it would no longer say which of the two the measurement reads. Keeping the
    /// movement identical leaves texture as the only difference, so the floor that separates
    /// them is measuring texture — which is the claim
    /// `a_raptor_scream_is_harsh_and_broadband_and_not_a_note` makes. `clean_squawk` is the
    /// same transform on the macaw, and keeps its arch for the same reason.
    fn clean_scream(call: Call, seed: u64) -> Sound {
        Sound {
            layers: call
                .description(seed)
                .layers
                .into_iter()
                .filter_map(|layer| match layer.exciter {
                    Exciter::Glide(glide) => Some(Layer {
                        exciter: Exciter::Glide(Glide {
                            wave: Wave::Sine,
                            ..glide
                        }),
                        filter: None,
                        ..layer
                    }),
                    _ => None,
                })
                .collect(),
        }
    }

    /// One cry's voiced layers alone, baked at 8 kHz **for the length the lane actually bakes
    /// it** — `profile().seconds`, not the glide's own `seconds`: the pitch without the breath,
    /// so a tracker reads the voice rather than the noise around it.
    ///
    /// **The length is the point, and it used to be the glide's.** Reading the contour over
    /// exactly the fall meant the measurement could not see what happened after the fall, or
    /// under a release that started before it ended — which is precisely the defect
    /// `a_raptor_cry_finishes_falling_before_its_release_begins` now catches, and which this
    /// test was structurally blind to while it chose its own duration. A test that renders a
    /// sound differently from the way the game renders it is measuring a different sound.
    fn voiced_scream(call: Call, seed: u64) -> Vec<f32> {
        Sound {
            layers: call
                .description(seed)
                .layers
                .into_iter()
                .filter(|layer| matches!(layer.exciter, Exciter::Glide(_)))
                .collect(),
        }
        .bake(call.profile().seconds, 8000, seed)
        .unwrap()
        .samples()
        .to_vec()
    }

    /// The call #1184 replaced, verbatim: two sine partials 29 Hz apart near 2.3 kHz under one
    /// band of noise. Two close sines beating is a tremolo on a tone, which is what the owner
    /// heard as an electronic buzz, and it is the negative control for every measurement below.
    fn old_rattlesnake(seed: u64) -> Sound {
        let variation = (seed % 101) as f32 / 100.0;
        let hz = 2300.0 + variation * 250.0;
        let envelope = Envelope {
            attack: 0.025,
            decay: 0.2,
            sustain: 0.7,
            release: 0.2,
        };
        let tone = |hz| Layer {
            exciter: Exciter::Oscillator {
                wave: Wave::Sine,
                hz,
            },
            gain: 0.08,
            envelope,
            filter: None,
            gate: None,
        };
        Sound {
            layers: vec![
                tone(hz),
                tone(hz + 29.0),
                Layer {
                    envelope,
                    gate: None,
                    ..noise(Noise::White, 0.24, FilterKind::Band, 2700.0, 2.0)
                },
            ],
        }
    }

    /// The old stack's three partials alone, baked as [`voiced_howl`] bakes the new voice, so
    /// both tracks are read by the same instrument from the same kind of input.
    fn voiced_old_wolf(seed: u64) -> Vec<f32> {
        Sound {
            layers: old_wolf(seed).layers[..3].to_vec(),
        }
        .bake(HOWL_SECONDS, HOWL_RATE, seed)
        .unwrap()
        .samples()
        .to_vec()
    }

    /// The two raptors and the figures each is built from.
    const RAPTORS: [(Call, &Scream); 2] = [(Call::Eagle, &EAGLE), (Call::Condor, &CONDOR)];

    /// The pitch a cry's glide starts from, read back from its description.
    fn scream_pitch(call: Call, seed: u64) -> f32 {
        match call.description(seed).layers[0].exciter {
            Exciter::Glide(glide) => glide.from,
            other => panic!("{call:?}'s first layer is not voiced: {other:?}"),
        }
    }

    /// #1186: the owner's rule is that a sound is realistic, never a note, and the eagle was
    /// the voice `a_squawk_is_harsh_and_broadband_and_not_a_note` had never been applied to —
    /// two clean sine partials, which whistle. Both raptors now spread their energy across the
    /// band they fill.
    ///
    /// **The negative controls are the point, and there are two per bird**: the cry voiced as
    /// clean sines along its own falling contour, and — for the eagle — the exact description
    /// it replaced. Each fails the measurement the cry passes, so the floor separates the two
    /// rather than passing everything put in front of it.
    #[test]
    fn a_raptor_scream_is_harsh_and_broadband_and_not_a_note() {
        for (call, _) in RAPTORS {
            let seconds = call.profile().seconds;
            for seed in (0..20u64).map(scramble) {
                let cry = call.bake(seed, 8000).unwrap();
                let flat = flatness(cry.samples(), 8000);
                let tonal = tonal_share(cry.samples(), 8000);
                assert!(
                    flat > 0.15 && tonal < 0.4,
                    "{call:?} seed {seed}: flatness {flat}, {tonal} of the energy on one \
                     frequency"
                );
                let clean = clean_scream(call, seed).bake(seconds, 8000, seed).unwrap();
                let mut notes = vec![("a clean scream", clean)];
                if call == Call::Eagle {
                    notes.push((
                        "the old eagle",
                        old_eagle(seed).bake(seconds, 8000, seed).unwrap(),
                    ));
                }
                for (name, note) in notes {
                    let flat = flatness(note.samples(), 8000);
                    assert!(
                        flat < 0.05,
                        "{call:?} seed {seed}: {name} measured flatness {flat}"
                    );
                }
            }
        }
        // And the two birds are not one description at another frequency: the condor spends
        // more of its level on air than on pitch, sits far below the eagle, and calls less
        // often. Each of the three is a number in its own [`Scream`] row or profile.
        //
        // The first two are `const` blocks because both rows are `const`: a build that made
        // the condor the brighter or the breathier of the pair would not compile, which is a
        // stronger claim than a test that has to be run and the one clippy asks for here.
        const {
            assert!(
                CONDOR.breath / CONDOR.voiced > EAGLE.breath / EAGLE.voiced * 2.0,
                "the condor is meant to be the hoarser of the two"
            );
        }
        const {
            assert!(
                CONDOR.hz + CONDOR.spread < EAGLE.hz * 0.5,
                "the condor is meant to be the lower of the two"
            );
        }
        assert!(
            Call::Condor.profile().interval[0] > Call::Eagle.profile().interval[1],
            "the condor is meant to be the sparser of the two"
        );
    }

    /// Both cries descend, and neither arches. A squawk rises into itself and falls out of it;
    /// a scream falls the whole way, which is the one contour difference between the two
    /// constructions. Read on the voiced layers alone, over the **whole baked call** and inside
    /// a band the fundamental stays in for the entire fall and its second harmonic never
    /// enters.
    ///
    /// Three claims, and the third only became readable once this stopped baking the glide's
    /// own length and started baking the profile's: the cry falls, it never arches, and after
    /// the fall it **holds its arrival** rather than drifting on — which is the audible half of
    /// `a_raptor_cry_finishes_falling_before_its_release_begins`.
    #[test]
    fn a_raptor_scream_falls_the_whole_way_through() {
        for (call, spec) in RAPTORS {
            for seed in (0..12u64).map(scramble) {
                let hz = scream_pitch(call, seed);
                let (low, high) = (hz * spec.fall * 0.9, hz * 1.15);
                let track = dominant_track(&voiced_scream(call, seed), 8000, low, high);
                // A reading pinned to either end of the search band is the tracker reporting
                // "no pitch found in here", not a pitch: `dominant_track` returns the best of a
                // 5 Hz grid, so when a window's spectrum is smeared — the eagle's 38 Hz rasp is
                // 1.1 cycles inside a 30 ms window, and the tail is quiet — the argmax can land
                // on the boundary. Four such windows out of sixty-six are what made the raw max
                // read 1191 Hz on a cry whose glide never exceeds 1078. They are dropped rather
                // than trusted; every window that carries a pitch is kept.
                let grid = 5.0;
                let track: Vec<(f32, f32)> = track
                    .into_iter()
                    .filter(|(_, hz)| *hz > low + grid && *hz < high - grid)
                    .collect();
                assert!(
                    track.len() >= 20,
                    "{call:?} seed {seed}: {} voiced windows",
                    track.len()
                );
                let quarter = track.len() / 4;
                let early = track[..quarter]
                    .iter()
                    .map(|(_, hz)| *hz)
                    .fold(0.0, f32::max);
                let late = track[track.len() - quarter..]
                    .iter()
                    .map(|(_, hz)| *hz)
                    .fold(f32::INFINITY, f32::min);
                assert!(
                    early >= late * 1.25,
                    "{call:?} seed {seed}: falls from {early} to only {late} Hz"
                );
                let (time, _) = track
                    .iter()
                    .copied()
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .unwrap();
                assert!(
                    time <= track[quarter].0,
                    "{call:?} seed {seed}: peaks at {time} s — a scream falls, it does not arch"
                );
                // The arrival is held: every window after the fall sits within a fifth of the
                // pitch the glide lands on, so the tail is a held note under a closing envelope
                // rather than a pitch still moving when the sound is taken away.
                let arrival = hz * spec.fall;
                for (time, pitch) in track.iter().filter(|(t, _)| *t > spec.seconds) {
                    assert!(
                        (pitch - arrival).abs() <= arrival * 0.2,
                        "{call:?} seed {seed}: {pitch} Hz at {time} s, {arrival} Hz expected"
                    );
                }
            }
        }
    }

    /// A cry reaches its lowest pitch before its close begins, with the arrival held open in
    /// between — the property both descriptions are written for and neither was checked
    /// against.
    ///
    /// **`Sound::bake` starts the release `release` seconds before the buffer ends**, so the
    /// three numbers that decide this live in three places: the glide's `seconds` in a
    /// [`Scream`] row, the envelope's `release` in `description`, and the buffer length in
    /// `profile`. Nothing connected them, and the eagle did not satisfy it — a 0.55 s fall
    /// and a 0.14 s release in a 0.65 s bake put the release 0.04 s *inside* the fall, so the
    /// cry was at 44% of its peak and dropping when it reached its lowest pitch. That is an
    /// envelope-shaped fall, which is the defect this issue replaced a description to remove;
    /// it survived as an arithmetic between three files (#1214 review).
    ///
    /// The margin is required to be positive rather than merely non-negative: a cry that
    /// begins closing at the exact instant it stops falling has no arrival, and "held open
    /// while the pitch falls, closed quickly" then describes a corner rather than a sound.
    #[test]
    fn a_raptor_cry_finishes_falling_before_its_release_begins() {
        for (call, spec) in RAPTORS {
            let profile = call.profile();
            // Every layer of a scream carries the one envelope, so any of them reports it —
            // and that they agree is worth asserting rather than assuming.
            let layers = call.description(7).layers;
            let release = layers[0].envelope.release;
            assert!(
                layers.iter().all(|layer| layer.envelope.release == release),
                "{call:?}'s layers close at different times"
            );
            let held = profile.seconds - release - spec.seconds;
            assert!(
                held > 0.0,
                "{call:?}: a {} s fall and a {release} s release in a {} s bake leave {held} s \
                 of held arrival — the release begins {} s before the fall ends",
                spec.seconds,
                profile.seconds,
                -held
            );
            // And it is an audible hold rather than a rounding: both birds carry 0.05 s.
            assert!(held >= 0.04, "{call:?} holds its arrival for only {held} s");
        }
    }

    /// Every frequency either raptor's description names stays at or under 3.6 kHz — 0.45 of
    /// the lowest supported rate — so both bake at an 8 kHz device rate. The glides are read
    /// at their widest: the top of the variation, the detuned twin, lifted by the rasp's
    /// depth.
    #[test]
    fn every_raptor_stays_under_the_lowest_nyquist_margin() {
        const MARGIN: f32 = 3600.0;
        for (call, spec) in RAPTORS {
            assert!(
                (spec.hz + spec.spread) * spec.detune * (1.0 + spec.rasp.depth) <= MARGIN,
                "{call:?}'s widest glide reach is over the margin"
            );
            for (band, _) in spec.formants.iter().chain(std::iter::once(&spec.hiss)) {
                assert!(*band <= MARGIN, "{call:?} has a {band} Hz band");
            }
            // And the description that is actually built, at both ends of the variation.
            for seed in [0, 100, 7, 0xfedc_ba98_7654_3210] {
                for layer in call.description(seed).layers {
                    if let Exciter::Glide(glide) = layer.exciter {
                        let reach = glide.from.max(glide.to) * (1.0 + glide.vibrato.depth);
                        assert!(reach <= MARGIN, "{call:?} glides to {reach} Hz");
                    }
                    if let Some(filter) = layer.filter {
                        assert!(filter.hz <= MARGIN, "{call:?} filters at {} Hz", filter.hz);
                    }
                }
                assert!(call.bake(seed, 8000).is_ok(), "{call:?} bakes at 8 kHz");
            }
        }
    }

    /// The band a howl's fundamental stays inside for the whole call and no partial of it ever
    /// enters: from below the lowest the falling line reaches to above the top of the arch.
    fn howl_band(seed: u64) -> (f32, f32) {
        let hz = howl_gesture(seed).hz;
        (hz * 0.46, hz * 1.45)
    }

    /// #1185: the howl was three sines at fixed frequencies whose upper partials faded in —
    /// the construction of a brass instrument, and what it sounded like. The pitch now moves:
    /// it rises into the call, holds within a few percent of its top for about a second, and
    /// falls away well below where it started, and no ten milliseconds of it jumps, which is
    /// the whinny's rule (#1160) at a slower tempo. The negative control is the point: the
    /// stack it replaced, quoted verbatim in [`old_wolf`], sits on one pitch and is read by the
    /// same tracker over the same band, so the floor separates the two rather than passing
    /// everything.
    #[test]
    fn a_howl_rises_holds_and_falls_where_a_static_stack_does_not() {
        for seed in howl_seeds() {
            let (low, high) = howl_band(seed);
            let track = pitch_track(&voiced_howl(seed), HOWL_RATE, low, high);
            assert!(
                track.len() >= 200,
                "seed {seed}: {} voiced windows",
                track.len()
            );
            let (first, top, time, last, hold) = contour(&track);
            assert!(
                top >= first * 1.10,
                "seed {seed}: rises from {first} to only {top} Hz"
            );
            assert!(
                top >= last * 1.20,
                "seed {seed}: falls from {top} to only {last} Hz"
            );
            assert!(
                (0.8..=2.4).contains(&time),
                "seed {seed}: tops out at {time} s"
            );
            assert!(hold >= 0.5, "seed {seed}: holds its top for only {hold} s");
            for pair in track.windows(2) {
                let step = (pair[1].1 / pair[0].1 - 1.0).abs();
                assert!(step < 0.06, "seed {seed}: a {step} step between {pair:?}");
            }
        }
        for seed in howl_seeds() {
            let (low, high) = howl_band(seed);
            let track = pitch_track(&voiced_old_wolf(seed), HOWL_RATE, low, high);
            let (first, top, _, last, _) = contour(&track);
            assert!(
                top < first * 1.02 && top < last * 1.02,
                "seed {seed}: the old stack moved from {first} through {top} to {last} Hz"
            );
        }
    }

    /// Two howls are two gestures rather than one gesture at two volumes: the rise differs in
    /// size and in when it tops out, and the fall differs in how far it goes. Measured from the
    /// rendered voice, not read back from the description, because a parameter that varies and
    /// never reaches the output is not a varying sound.
    ///
    /// Across the eight scrambled seeds the rise spans 1.135 to 1.267, the top falls between
    /// 1.29 and 1.56 s, and the fall spans 1.256 to 1.412 — ratios of 1.12, 1.21 and 1.12. The
    /// floors below sit under each, and the point of having three of them is that a gesture
    /// cannot satisfy them all by being loud. The three pinned seeds run here as well and land
    /// in the same bands; what they are here to guard is the block at the end of this test.
    #[test]
    fn no_two_howls_make_the_same_gesture() {
        let measured: Vec<(f32, f32, f32)> = howl_seeds()
            .map(|seed| {
                let (low, high) = howl_band(seed);
                let track = pitch_track(&voiced_howl(seed), HOWL_RATE, low, high);
                let (first, top, time, last, _) = contour(&track);
                (top / first, time, top / last)
            })
            .collect();
        let spread = |pick: fn(&(f32, f32, f32)) -> f32| {
            let values: Vec<f32> = measured.iter().map(pick).collect();
            let (low, high) = (
                values.iter().copied().fold(f32::MAX, f32::min),
                values.iter().copied().fold(f32::MIN, f32::max),
            );
            high / low
        };
        assert!(spread(|g| g.0) > 1.05, "every rise is the same size");
        assert!(
            spread(|g| g.1) > 1.10,
            "every rise tops out at the same time"
        );
        assert!(spread(|g| g.2) > 1.05, "every fall is the same size");

        // And the pinned seeds in particular — the seeds a regression in [`spread`] is the only
        // thing that could reach, since the spreads above are measured over scrambled seeds
        // whose high bytes are set and which a bare shift distributes perfectly well.
        //
        // This one assertion reads the gesture's parameters rather than the rendered triple, and
        // that is a measurement rather than a preference. Seeds `0x0` and `0xfedcba9876543210`
        // come out only 1.3% apart in the rendered rise and 1.3% in the time of the top, because
        // `spread` happens to hand them a near-identical arch (0.36 against 0.35) and span (3.44
        // against 3.43) — a collision by luck, not by construction, and harmless because a seed
        // reaching this from the lane is scrambled first. But it means a rendered-triple guard
        // would have to sit under 1%, which is no separation at all from the 0.8% a reverted
        // `spread` leaves, so the rendered numbers cannot be what carries this claim.
        //
        // The parameters can, because a revert is exact there rather than approximate: read
        // straight off the raw seed, `0x0` and `0x10203` share span, onset and close outright
        // and differ in arch alone (0.22 against 0.23). One differing field of four *is* the
        // collapse, so two is the floor — and that fails on the revert while every assertion
        // above it still passes. The rendered half of the claim, that the variety reaches the
        // output at all, is what the three spreads above measure, over these seeds included.
        let pinned: Vec<Howl> = super::super::pins::SEEDS
            .into_iter()
            .map(howl_gesture)
            .collect();
        for (index, one) in pinned.iter().enumerate() {
            for other in &pinned[index + 1..] {
                let differing = [
                    one.arch != other.arch,
                    one.span != other.span,
                    one.onset != other.onset,
                    one.close != other.close,
                ]
                .into_iter()
                .filter(|differs| *differs)
                .count();
                assert!(
                    differing >= 2,
                    "two pinned seeds agree in {} of four gesture fields",
                    4 - differing
                );
            }
        }
    }

    /// The owner's rule is that a sound is realistic, never a note (#1161, #1176), and the howl
    /// was an exact harmonic series of sines. It is now a voice: three formant bands over
    /// textured waves, with breath through the same bands filling the spectrum between the
    /// harmonics. The negative control is the stack it replaced, which keeps its energy on a
    /// few frequencies with nothing between them and fails the same measurement.
    ///
    /// The floor is 0.02 rather than the squawk's 0.15, and the reason is the length of the
    /// call rather than anything about the voice: [`flatness`] takes one periodogram of the
    /// whole buffer, so 3.8 s at 8 kHz is twelve thousand bins where a 0.85 s squawk is under
    /// three, and a harmonic that fills a bin in the short call is a spike between empty ones
    /// in the long one. What matters is the separation, which is wide and measured: the eight
    /// scrambled seeds come out between 0.043 and 0.294, the three pinned ones inside that, and
    /// the two controls at 0.0013 and 0.0000.
    #[test]
    fn a_howl_is_a_voiced_throat_and_not_a_harmonic_stack() {
        for seed in howl_seeds() {
            let call = Call::Wolf.bake(seed, 8000).unwrap();
            let flat = flatness(call.samples(), 8000);
            let tonal = tonal_share(call.samples(), 8000);
            assert!(
                flat > 0.02 && tonal < 0.4,
                "seed {seed}: flatness {flat}, {tonal} of the energy on one frequency"
            );
            let old = old_wolf(seed).bake(HOWL_SECONDS, 8000, seed).unwrap();
            let stack = Sound {
                layers: old_wolf(seed).layers[..3].to_vec(),
            }
            .bake(HOWL_SECONDS, 8000, seed)
            .unwrap();
            for (name, note) in [("the old call", old), ("its sines alone", stack)] {
                let flat = flatness(note.samples(), 8000);
                assert!(flat < 0.005, "seed {seed}: {name} measured flatness {flat}");
            }
        }
    }

    /// Heard where the night lane places it — twenty-six blocks out and level, faded by the
    /// same `spatial::attenuation` every placed sound is. Every frequency the description names
    /// or reaches stays at or under 3.6 kHz, which is what lets it bake at an 8 kHz device at
    /// all; it never clips, and starts and ends at exact silence, at every device rate.
    ///
    /// The heard floor is 0.03 and not the macaw's 0.06, because twenty-six blocks on a
    /// 96-block range is `attenuation(26, 96) == 0.0573`: a sound that peaked at the 0.85 this
    /// test forbids would still only be heard at 0.0487, so 0.06 is not a level the wolf's
    /// placement can reach at all, and a test asserting it would be asserting about the lane
    /// rather than about the call. 0.03 is 62% of the most anything can be heard at from there,
    /// and the howl comes out between 0.034 and 0.042 — 0.59 to 0.73 before placement, the
    /// lower end at 192 kHz. No claim is made here that it is louder than the stack it
    /// replaced, which peaked at 0.585 and was heard at 0.0335: it sits in the same band, and a
    /// comparison of two peaks is not a comparison of two loudnesses anyway — the old call was
    /// three bare sines and this one is a voice behind formant bands.
    #[test]
    fn a_wolf_call_carries_to_its_range_without_clipping_at_any_rate() {
        let profile = Call::Wolf.profile();
        let gain = spatial::attenuation(profile.radius.hypot(profile.height), profile.range);
        for seed in howl_seeds() {
            for layer in &Call::Wolf.description(seed).layers {
                // `Glide::peak`, which gates the bake, is private to `audio::synth`; the highest
                // frequency a glide reaches is its further end lifted by the whole arch.
                let top = match layer.exciter {
                    Exciter::Glide(glide) => glide.from.max(glide.to) * (1.0 + glide.vibrato.depth),
                    _ => 0.0,
                };
                let band = layer.filter.map_or(0.0, |filter| filter.hz);
                assert!(
                    top <= 3600.0 && band <= 3600.0,
                    "seed {seed}: a glide reaching {top} Hz through a {band} Hz band"
                );
            }
            for rate in [8000, 44100, 48000, 96000, 192000] {
                let call = Call::Wolf.bake(seed, rate).unwrap();
                let samples = call.samples();
                assert!(samples.iter().all(|v| v.is_finite()));
                assert!(
                    peak(samples) < 0.85,
                    "seed {seed} at {rate}: peaks at {}",
                    peak(samples)
                );
                assert!(
                    peak(samples) * gain >= 0.03,
                    "seed {seed} at {rate}: heard at {}",
                    peak(samples) * gain
                );
                assert_eq!(samples.first(), Some(&0.0));
                assert_eq!(samples.last(), Some(&0.0));
            }
        }
    }

    /// The gate the rattle's description carries, read back from it. Every layer carries the
    /// same one, which is what makes the three bands one click rather than three.
    fn rattle_gate(seed: u64) -> Gate {
        let layers = Call::Rattlesnake.description(seed).layers;
        let gate = layers[0].gate.expect("a rattle is gated");
        for layer in &layers {
            assert_eq!(layer.gate, Some(gate), "the bands do not share one gate");
            assert!(
                matches!(layer.exciter, Exciter::Noise(Noise::White)),
                "a rattle has a pitch: {:?}",
                layer.exciter
            );
        }
        gate
    }

    /// Where each click of a rendered rattle starts. Every layer carries the same gate, so a
    /// closed gate is applied after the filter and is exact zero in the sum; a click is a run
    /// of sounding samples after at least four such zeros. The real gaps are four fifths of a
    /// period — seventy samples at 8 kHz and four hundred at 48 — so four is a margin against
    /// a sum that happens to land on zero mid-click, not a threshold anything depends on.
    fn rendered_clicks(samples: &[f32]) -> Vec<usize> {
        let mut starts = Vec::new();
        let mut silence = usize::MAX;
        for (index, value) in samples.iter().enumerate() {
            if *value == 0.0 {
                silence = silence.saturating_add(1);
            } else {
                if silence >= 4 {
                    starts.push(index);
                }
                silence = 0;
            }
        }
        starts
    }

    /// The amplitude envelope of `samples`, decimated to a thousand readings a second: the
    /// absolute value through a one-pole at 250 Hz, kept every `rate / 1000`th sample. A click
    /// train's envelope is a periodic train in its own right; a held band's is a nearly steady
    /// level carrying the noise's own wideband flutter.
    fn amplitude_envelope(samples: &[f32], rate: u32) -> Vec<f32> {
        let pole = 1.0 - (-std::f32::consts::TAU * 250.0 / rate as f32).exp();
        let step = (rate / 1000) as usize;
        let mut level = 0.0;
        samples
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                level += pole * (value.abs() - level);
                (index % step == 0).then_some(level)
            })
            .collect()
    }

    /// The share of an amplitude envelope's *varying* energy that lies between `low` and `high`
    /// hertz, out of everything from 5 Hz to 480. This is the measurement that separates a train
    /// of impacts from a held band: a gate puts nearly all of that energy on its own rate and
    /// that rate's harmonics, while a filtered band of noise has no rate at all and spreads the
    /// same energy thinly across the whole range. The mean is removed first, so a loud sound and
    /// a quiet one of the same shape measure alike.
    fn modulation_share(samples: &[f32], rate: u32, low: f32, high: f32) -> f64 {
        let envelope = amplitude_envelope(samples, rate);
        let mean = envelope.iter().sum::<f32>() / envelope.len() as f32;
        let varying: Vec<f32> = envelope.iter().map(|value| value - mean).collect();
        let inside: f64 = band_power(&varying, 1000, low, high).iter().sum();
        let across: f64 = band_power(&varying, 1000, 5.0, 480.0).iter().sum();
        inside / across
    }

    /// Heard where the day lane places it — seven blocks out and five up, faded by the same
    /// `spatial::attenuation` every placed sound is. A macaw carries: the squawk peaks near 0.6
    /// before placement at 48 kHz and near 0.44 at 8 kHz, where the call it replaced peaked near
    /// 0.23 and was heard at about 0.04. It never clips, and starts and ends at silence, at
    /// every device rate.
    #[test]
    fn a_macaw_call_carries_to_its_perch_without_clipping_at_any_rate() {
        let profile = Call::Parrot.profile();
        let gain = spatial::attenuation(profile.radius.hypot(profile.height), profile.range);
        for seed in (0..20u64).map(scramble) {
            for rate in [8000, 44100, 48000, 96000, 192000] {
                let call = Call::Parrot.bake(seed, rate).unwrap();
                let samples = call.samples();
                assert!(samples.iter().all(|v| v.is_finite()));
                assert!(
                    peak(samples) < 0.85,
                    "seed {seed} at {rate}: peaks at {}",
                    peak(samples)
                );
                assert!(
                    peak(samples) * gain >= 0.06,
                    "seed {seed} at {rate}: heard at {}",
                    peak(samples) * gain
                );
                assert_eq!(samples.first(), Some(&0.0));
                assert_eq!(samples.last(), Some(&0.0));
            }
        }
    }

    /// #1184: the owner's rule is that a sound is realistic, never a note, and the desert's day
    /// call was two sines 29 Hz apart beating under a band of noise — a tremolo on a tone, which
    /// is why it read as an electronic buzz. A rattle is a train of impacts: its energy sits in
    /// its own *modulation*, on the gate's rate and that rate's harmonics, and the gaps between
    /// its clicks are silence rather than a dip. The negative control is the point: the call it
    /// replaced, quoted verbatim, fails both halves of the measurement — it has no clicks to
    /// count, and what modulation it has is the noise band's own flutter plus a 29 Hz beat,
    /// spread across the whole range instead of concentrated on a rate.
    #[test]
    fn a_rattle_is_a_train_of_dry_clicks_and_not_a_beating_pair_of_sines() {
        let seconds = Call::Rattlesnake.profile().seconds;
        for seed in (0..16u64).map(scramble) {
            for rate in [8000, 48000] {
                let call = Call::Rattlesnake.bake(seed, rate).unwrap();
                let clicks = rendered_clicks(call.samples());
                let density = clicks.len() as f32 / seconds;
                assert!(
                    (40.0..=90.0).contains(&density),
                    "seed {seed} at {rate}: {density} clicks a second"
                );
            }
            let call = Call::Rattlesnake.bake(seed, 8000).unwrap();
            let samples = call.samples();
            // Measured over forty seeds: 0.73 to 0.79 of the modulation sits on the click rate
            // and its harmonics, where the call this replaced reaches at most 0.37. And a rattle
            // has no pitch at all — at most 0.08 of its energy on one frequency against the old
            // call's 0.65 at least, a flatness of 0.45 at worst against the old call's 0.07.
            let share = modulation_share(samples, 8000, 30.0, 200.0);
            let (tonal, flat) = (tonal_share(samples, 8000), flatness(samples, 8000));
            assert!(
                share > 0.6 && tonal < 0.2 && flat > 0.3,
                "seed {seed}: modulation {share}, {tonal} on one frequency, flatness {flat}"
            );
            let old = old_rattlesnake(seed).bake(seconds, 8000, seed).unwrap();
            let old = old.samples();
            let density = rendered_clicks(old).len() as f32 / seconds;
            let share = modulation_share(old, 8000, 30.0, 200.0);
            let (tonal, flat) = (tonal_share(old, 8000), flatness(old, 8000));
            assert!(
                density < 40.0 && share < 0.5 && tonal > 0.4 && flat < 0.15,
                "seed {seed}: the old call measured {density} clicks a second, modulation \
                 {share}, {tonal} on one frequency, flatness {flat}"
            );
        }
    }

    /// The rattle is wound up rather than switched on: the click rate climbs and the level rises
    /// with it, and both come off the seed, so two rattlesnakes heard in one crossing are not
    /// the same recording. The gate's three numbers are read from independent slices of the seed
    /// — the rate it starts at, the rate it reaches, and how long it takes — so the span and its
    /// duration are not one number wearing two hats.
    #[test]
    fn a_rattle_winds_up_and_falls_away_and_no_two_seeds_wind_up_alike() {
        let mut winds = std::collections::HashSet::new();
        for seed in (0..40u64).map(scramble) {
            let gate = rattle_gate(seed);
            assert!(
                gate.from >= RATTLE_SLOWEST
                    && gate.from < gate.to
                    && gate.to <= RATTLE_FASTEST
                    && (0.2..0.6).contains(&gate.seconds),
                "seed {seed}: {gate:?}"
            );
            winds.insert((
                gate.from.to_bits(),
                gate.to.to_bits(),
                gate.seconds.to_bits(),
            ));

            let call = Call::Rattlesnake.bake(seed, 8000).unwrap();
            let samples = call.samples();
            // The train speeds up: the last five intervals are shorter than the first five.
            let gaps: Vec<usize> = rendered_clicks(samples)
                .windows(2)
                .map(|pair| pair[1] - pair[0])
                .collect();
            let mean = |gaps: &[usize]| gaps.iter().sum::<usize>() as f32 / gaps.len() as f32;
            let (early, late) = (mean(&gaps[..5]), mean(&gaps[gaps.len() - 5..]));
            assert!(
                late < early * 0.92,
                "seed {seed}: {early} samples between early clicks, {late} between late ones"
            );
            // And the level rises out of nothing and falls away again.
            let rms = |from: f32, to: f32| {
                let window = &samples[(from * 8000.0) as usize..(to * 8000.0) as usize];
                (window.iter().map(|v| v * v).sum::<f32>() / window.len() as f32).sqrt()
            };
            let held = rms(0.35, 0.45);
            assert!(
                rms(0.0, 0.06) < held * 0.75 && rms(0.74, 0.8) < held * 0.75,
                "seed {seed}: {} then {held} then {}",
                rms(0.0, 0.06),
                rms(0.74, 0.8)
            );
        }
        // All forty are distinct as measured; the floor leaves room for a collision rather than
        // asserting a property of these particular seeds.
        assert!(winds.len() >= 36, "{} distinct wind-ups in 40", winds.len());
    }

    /// Heard where the lane places it — five blocks out and a block down in the sand, faded by
    /// the same `spatial::attenuation` every placed sound is. It never clips and starts and ends
    /// at exact silence at every device rate, 8 kHz included: that the 8 kHz bake succeeds at all
    /// is the proof that no band and no gate rate crosses its bound.
    ///
    /// The two floors are measured over forty seeds at five rates. The peak reaches 0.79 at
    /// worst, against the 0.85 asserted. Heard, it is 0.195 at 8 kHz and 0.176 at 48 — where the
    /// call it replaced was about 0.105 — and falls to 0.059 at 192 kHz, because a band of white
    /// noise carries less amplitude per sample the finer the sample grid is. That worst case is
    /// what the 0.055 floor sits under, and it is the reason this one number is below the 0.06
    /// the macaw's test uses rather than equal to it.
    #[test]
    fn a_rattlesnake_call_carries_to_its_coil_without_clipping_at_any_rate() {
        let profile = Call::Rattlesnake.profile();
        let gain = spatial::attenuation(profile.radius.hypot(profile.height), profile.range);
        for seed in (0..20u64).map(scramble) {
            for rate in [8000, 44100, 48000, 96000, 192000] {
                let call = Call::Rattlesnake.bake(seed, rate).unwrap();
                let samples = call.samples();
                assert!(samples.iter().all(|v| v.is_finite()));
                assert!(
                    peak(samples) < 0.85,
                    "seed {seed} at {rate}: peaks at {}",
                    peak(samples)
                );
                assert!(
                    peak(samples) * gain >= 0.055,
                    "seed {seed} at {rate}: heard at {}",
                    peak(samples) * gain
                );
                assert_eq!(samples.first(), Some(&0.0));
                assert_eq!(samples.last(), Some(&0.0));
            }
        }
    }
}
