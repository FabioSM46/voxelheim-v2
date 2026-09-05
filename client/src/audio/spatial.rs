//! Where a voice is, expressed as numbers a mixer source can be set to.
//!
//! **Every function here is pure, and that is the point of the module rather than a
//! property of it.** Spatialisation is presentation — `client/AGENTS.md` says nothing under
//! `audio/` may be read by input, targeting, placement or anything else that decides an
//! outcome, and a gain is not a fact about the world. What follows is arithmetic over a
//! position, an angle and an occlusion value; it reads no resource, holds no state and
//! answers the same thing every time it is asked.
//!
//! ```text
//!   listener eye, camera yaw, speaker eye
//!            │
//!            ├─ distance ──▶ attenuation()  ─┐
//!            │                               ├─▶ PanGains  ──▶ one mixer source
//!            └─ azimuth  ──▶ pan_gains()    ─┘
//!                       └──▶ back_cue()  ──┐
//!                                          ├─▶ band gains ──▶ that source's filter
//!               occlusion ──▶ band_gains() ┘
//! ```
//!
//! ## Two cues, two mechanisms, and why the pan law needs help
//!
//! [`pan_gains`] is a constant-power law over the *lateral* component of the azimuth, so a
//! source directly ahead and a source directly behind produce exactly the same pair of
//! gains. That is not a defect to be papered over: two loudspeakers in front of a listener
//! have no way to place a sound behind them, and pretending otherwise is what HRTF is for
//! — which `docs/adr/0001-voice-transport.md`'s dependency budget and this issue's Out of
//! Scope both decline. [`back_cue`] is the cheap substitute the ear actually accepts: a
//! head shadows high frequencies from behind, so a voice behind the listener loses some of
//! its top. It is a multiplier on the high band of the same three-band filter occlusion
//! drives, and it is applied *on top of* whatever occlusion asked for rather than instead
//! of it.
//!
//! ## Time is a parameter here, never a clock
//!
//! [`advance`] takes the seconds that have passed and returns the new value. It reads no
//! `Instant` and owns no state, because its one caller is the output callback — which may
//! not allocate, lock, log or touch a Bevy type, and which knows exactly how long the block
//! it is about to render lasts. Where the state lives is `audio/mixer.rs`'s problem; how
//! far it may move in a block is this module's.

use std::f32::consts::{FRAC_PI_4, TAU};

use bevy::prelude::Vec2;
use bevy::prelude::Vec3;

/// How close a speaker may be before distance takes anything away, in blocks.
///
/// Inside it a voice is at full gain. Two blocks is about arm's length in this world and
/// is short enough that "next to me" is a small place rather than a room; the constant
/// exists mostly so the inverse-distance curve below has somewhere finite to start, since
/// `1/d` has no value at zero.
pub const FULL_GAIN_BLOCKS: f32 = 2.0;

/// What one voice's gain is at `distance`, on a server that relays voice `range_blocks` far.
///
/// Full gain out to [`FULL_GAIN_BLOCKS`], then an inverse-distance rolloff that reaches
/// **exactly** zero at `range_blocks`. Both halves of that sentence are deliberate:
///
/// - *Inverse distance* is how sound actually thins out, and it is what makes the first few
///   blocks of walking away cost far more than the last few. A linear ramp over the same
///   span sounds like somebody turning a knob.
/// - *Exactly zero at the range* is what stops a voice from being cut off mid-word. The
///   server decides who is audible from `range_blocks` (`schemas/handshake.fbs`), so a
///   curve that was still at 0.3 when the server stopped relaying would end every departure
///   with a click. The raw `1/d` curve is therefore shifted and rescaled so its value at the
///   range is the zero it is already going to be given.
///
/// `range_blocks` is the server's number, never a constant here. Zero means a server that
/// relays no voice at all, and answers zero — that state reaches this function only through
/// a frame that cannot exist, and answering anything else would be inventing audibility the
/// server declined to grant.
///
/// **Inverse distance is steep, and that is worth stating rather than discovering.** On a
/// 32-block server this curve is at 0.47 four blocks out, 0.20 at eight and 0.07 at sixteen
/// — so the half of the range beyond eight blocks is faint rather than merely quieter. That
/// is what `1/d` is, and it is why the server's range and the audible range are not the
/// same number: the server relays generously and the ear decides. A gentler curve would be
/// a different acceptance criterion, not a tidier implementation of this one.
///
/// Non-finite inputs answer zero rather than being clamped, which is `client/AGENTS.md`'s
/// rule: `NaN` compares false against every bound, so a clamp passes it straight through
/// into a gain a device would have to render.
pub fn attenuation(distance: f32, range_blocks: f32) -> f32 {
    if !distance.is_finite() || !range_blocks.is_finite() || range_blocks <= 0.0 {
        return 0.0;
    }
    let distance = distance.max(0.0);
    if distance >= range_blocks {
        return 0.0;
    }
    if distance <= FULL_GAIN_BLOCKS {
        return 1.0;
    }
    // `raw(FULL_GAIN_BLOCKS)` is 1.0 by construction, so the shift only has to remove the
    // curve's value at the range. The two early returns above are what guarantee the
    // denominator is positive: reaching here means `FULL_GAIN_BLOCKS < distance <
    // range_blocks`, so `raw(range_blocks) < 1.0`.
    let raw = FULL_GAIN_BLOCKS / distance;
    let at_range = FULL_GAIN_BLOCKS / range_blocks;
    ((raw - at_range) / (1.0 - at_range)).clamp(0.0, 1.0)
}

/// Where the speaker is, as an angle around the listener: `0` dead ahead, positive to the
/// **right**, `±π` behind.
///
/// Horizontal only. A voice above or below the listener is a voice in the same direction as
/// far as two loudspeakers are concerned, and folding height into the angle would swing a
/// voice across the stereo field as somebody climbed a ladder directly overhead.
///
/// `yaw` is the camera's, in the client's own convention: `Quat::from_rotation_y(yaw)`
/// rotates Bevy's `-Z` forward, so forward is `(-sin yaw, -cos yaw)` and right is
/// `(cos yaw, -sin yaw)` in the XZ plane. `player/camera.rs` builds the camera's rotation
/// from exactly that quaternion, which is why this takes the angle rather than a transform:
/// a `Transform` also carries pitch, and the horizontal forward extracted from a
/// steeply-pitched one is a tiny vector to normalise.
///
/// A speaker standing exactly on the listener has no direction, and answers `0` — dead
/// ahead, which is where [`pan_gains`] puts a sound it cannot place.
pub fn azimuth(listener: Vec3, yaw: f32, speaker: Vec3) -> f32 {
    if !listener.is_finite() || !speaker.is_finite() || !yaw.is_finite() {
        return 0.0;
    }
    let to = Vec2::new(speaker.x - listener.x, speaker.z - listener.z);
    if to.x == 0.0 && to.y == 0.0 {
        return 0.0;
    }
    let forward = Vec2::new(-yaw.sin(), -yaw.cos());
    let right = Vec2::new(yaw.cos(), -yaw.sin());
    to.dot(right).atan2(to.dot(forward))
}

/// One source's two output gains, before the bus arithmetic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanGains {
    pub left: f32,
    pub right: f32,
}

impl PanGains {
    /// What a speaker with no position is heard at: the same sample in both ears, at unity.
    ///
    /// **Deliberately not a point on the pan law.** [`pan_gains`] is constant *power*, so
    /// its centre is `1/√2` in each ear and the pair sums to one unit of power; this is the
    /// absence of a position rather than a position in the middle, and the acceptance
    /// criterion for it is "unpositioned mono at bus gain" — which is the gain the mixer
    /// applied before any of this existed, when it wrote one mono sum to every channel.
    /// A speaker the snapshot has not placed is therefore heard exactly as they were
    /// before, and not 3 dB quieter than one standing in front of the listener.
    pub const UNPOSITIONED: Self = Self {
        left: 1.0,
        right: 1.0,
    };
}

/// The constant-power pan for a source at `azimuth`.
///
/// `left² + right² == 1` at every angle, which is what makes a voice keep its apparent
/// loudness while it crosses the stereo field. The alternative — gains that sum to one —
/// dips audibly in the middle, because two uncorrelated halves at 0.5 are quieter than one
/// whole.
///
/// Only the **lateral** component of the angle is a pan: `sin(azimuth)` is `+1` due right,
/// `-1` due left, and `0` both dead ahead and directly behind. Front and back are
/// deliberately identical here — see the module doc — and [`back_cue`] is what tells them
/// apart.
pub fn pan_gains(azimuth: f32) -> PanGains {
    if !azimuth.is_finite() {
        return PanGains::UNPOSITIONED;
    }
    let lateral = azimuth.sin().clamp(-1.0, 1.0);
    // Maps [-1, 1] onto [0, π/2], so cosine walks 1 → 0 while sine walks 0 → 1.
    let theta = (lateral + 1.0) * FRAC_PI_4;
    // `max(0.0)` and not decoration: `cos(π/2)` is `-4.4e-8` in `f32`, and a negative gain
    // is a channel played backwards rather than a quiet one. The clamp costs nothing the
    // pan law can measure and removes a sign the mixer would faithfully render.
    PanGains {
        left: theta.cos().max(0.0),
        right: theta.sin().max(0.0),
    }
}

/// How much of the high band survives at the back of the listener's head.
///
/// Half. A head is not a wall, and a voice behind somebody is still plainly a voice — this
/// is the cue that says "behind", not an obstruction. It is a good deal less than
/// [`OCCLUDED_HIGH`] takes, which is the correct ordering: standing behind a listener must
/// never sound like standing behind a rock.
pub const BACK_HIGH_CUT: f32 = 0.5;

/// The multiplier the front/back cue puts on the high band alone, from `1.0` dead ahead to
/// `1.0 - `[`BACK_HIGH_CUT`] directly behind.
///
/// **This is the "gentle low-pass" of the acceptance criterion, and it is worth being exact
/// about the mechanism**: there is no second filter. The source already runs through the
/// three-band split occlusion drives, and taking the top band down while leaving the rest
/// alone *is* a first-order low-pass shelf — one more multiply in the render path rather
/// than one more filter with state of its own. Nothing else in the chain is touched, so a
/// voice behind the listener is as loud and as close as it was; it has only lost its edge.
///
/// Symmetric in sign, because a head is: `back_cue(θ) == back_cue(-θ)`.
pub fn back_cue(azimuth: f32) -> f32 {
    if !azimuth.is_finite() {
        return 1.0;
    }
    // 0 dead ahead, 1 directly behind, and a cosine rather than an absolute angle so the
    // cue arrives gradually across the sides instead of switching on at ±π/2.
    let behind = (1.0 - azimuth.cos()) * 0.5;
    1.0 - BACK_HIGH_CUT * behind
}

/// How many bands the occlusion filter splits a source into.
pub const BANDS: usize = 3;

/// What the low band keeps when a voice is completely occluded.
///
/// Low frequencies go through a wall; that is why a neighbour's music is bass. Taking 30%
/// keeps the voice plainly present while making it unmistakably muffled.
pub const OCCLUDED_LOW: f32 = 0.70;
/// What the mid band keeps when a voice is completely occluded. Most of the intelligibility
/// of speech lives here, which is why this is where the biggest audible change is.
pub const OCCLUDED_MID: f32 = 0.25;
/// What the high band keeps when a voice is completely occluded. Nearly nothing: consonants
/// are the first thing a wall takes.
pub const OCCLUDED_HIGH: f32 = 0.05;

/// Standing behind a listener must not sound like standing behind a rock. The two cues
/// share the high band, so the only thing keeping them from being confusable is that the
/// head takes less of it than a wall does — checked at compile time, because it is a
/// relation between two constants and nothing at run time can change it.
const _: () = assert!(1.0 - BACK_HIGH_CUT > OCCLUDED_HIGH);

/// The per-band gains for an occlusion of `occlusion`, as `[low, mid, high]`.
///
/// Linear in the occlusion, from `[1, 1, 1]` at zero to the three `OCCLUDED_*` constants at
/// one. Linear rather than shaped because the occlusion value it is given is already a mean
/// of weighted materials — putting a curve on top of a weighted mean makes the weights mean
/// something nobody can state.
///
/// The ordering is the whole behaviour: each band falls, and the higher the band the faster
/// it falls. A filter whose three bands moved together would be a volume control, and it
/// would pass any test that only checked the gains went down.
pub fn band_gains(occlusion: f32) -> [f32; BANDS] {
    let occlusion = if occlusion.is_finite() {
        occlusion.clamp(0.0, 1.0)
    } else {
        0.0
    };
    [
        1.0 - (1.0 - OCCLUDED_LOW) * occlusion,
        1.0 - (1.0 - OCCLUDED_MID) * occlusion,
        1.0 - (1.0 - OCCLUDED_HIGH) * occlusion,
    ]
}

/// How long the smoothed occlusion takes to travel the whole `0..1` range while rising.
pub const OCCLUSION_ATTACK_SECONDS: f32 = 0.050;
/// And while falling. Six times as long, which is the asymmetry that keeps a voice from
/// flickering: stepping behind a pillar should be immediate, stepping back out should not
/// chatter if the rays disagree for a moment about a doorway's edge.
pub const OCCLUSION_RELEASE_SECONDS: f32 = 0.300;

/// Moves `current` towards `target` by at most what `elapsed_seconds` allows.
///
/// **A linear ramp over the full `0..1` range, not a one-pole exponential**, and the choice
/// is about what the two constants above are allowed to mean. A time constant reaches 63%
/// of the way and never arrives; a full-travel time arrives, exactly, and can be asserted:
/// five 10 ms blocks take a voice from open air to fully occluded and thirty take it back,
/// which is a sentence a test can fail.
///
/// The step is a fraction of the whole range rather than of the remaining distance, so a
/// small change is fast and a large one takes the stated time — the behaviour a listener
/// notices is the wall, and the wall is the large change.
///
/// Answers `current` unchanged for a non-positive or non-finite block length, so a device
/// reporting nonsense cannot move a filter.
pub fn advance(current: f32, target: f32, elapsed_seconds: f32) -> f32 {
    let current = if current.is_finite() {
        current.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
        return current;
    }
    let target = if target.is_finite() {
        target.clamp(0.0, 1.0)
    } else {
        return current;
    };
    let full_travel = if target > current {
        OCCLUSION_ATTACK_SECONDS
    } else {
        OCCLUSION_RELEASE_SECONDS
    };
    let step = elapsed_seconds / full_travel;
    current + (target - current).clamp(-step, step)
}

/// Where the low band ends, in hertz.
///
/// 300 Hz is under the first formant of a speaking voice, so what is below it is body
/// rather than words — which is exactly the part a wall lets through.
pub const LOW_CROSSOVER_HZ: f32 = 300.0;
/// And where the high band begins. Above 3 kHz is sibilance and consonant edge: the part a
/// wall takes first and the part a head shadows.
pub const HIGH_CROSSOVER_HZ: f32 = 3_000.0;

/// The coefficient of a one-pole low-pass at `hz`, for a stream running at `sample_rate`.
///
/// Used as `state += coefficient * (sample - state)`, so `0` holds the filter still and `1`
/// makes it a wire. `1 - exp(-2π f / sr)` is the standard exponential-decay form, which is
/// what makes the answer depend on the device's rate rather than on an assumed 48 kHz.
///
/// **Computed on the Bevy side, once per stream**, and stored for the callback to load —
/// which is the whole reason it is a function of the rate rather than of the block. `exp`
/// is arithmetic and would be legal in the callback, but it would be arithmetic repeated
/// for every source on every block to answer a question whose inputs change only when a
/// device is opened.
///
/// A rate of zero, or a non-finite frequency, answers `1.0`: a filter that is a wire. That
/// is the safe degenerate rather than the neutral one — with both crossovers at `1.0` the
/// low band is the whole signal and the other two are empty, so the three-band stage
/// collapses to the low band's gain and a voice stays audible instead of vanishing.
pub fn one_pole_coefficient(hz: f32, sample_rate: u32) -> f32 {
    if !hz.is_finite() || sample_rate == 0 {
        return 1.0;
    }
    1.0 - (-TAU * hz.max(0.0) / sample_rate as f32).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, PI};

    /// The range a test server relays voice over, in blocks.
    const RANGE: f32 = 32.0;

    #[test]
    fn a_voice_is_at_full_gain_until_it_is_two_blocks_away() {
        assert_eq!(attenuation(0.0, RANGE), 1.0);
        assert_eq!(attenuation(1.0, RANGE), 1.0);
        assert_eq!(attenuation(FULL_GAIN_BLOCKS, RANGE), 1.0);
        assert!(attenuation(FULL_GAIN_BLOCKS + 0.5, RANGE) < 1.0);
    }

    #[test]
    fn a_voice_reaches_exactly_zero_at_the_servers_range() {
        assert_eq!(attenuation(RANGE, RANGE), 0.0);
        assert_eq!(attenuation(RANGE + 1.0, RANGE), 0.0);
        // And it arrives there smoothly rather than falling off a step: the last audible
        // sample before the range is quiet, which is what stops the server's cutoff from
        // being a click.
        assert!(attenuation(RANGE - 0.01, RANGE) < 0.01);
    }

    /// The curve has to be *inverse distance*, and a linear ramp would pass every
    /// monotonicity assertion above. This is the one that tells them apart: a linear ramp is
    /// at exactly half its span halfway along it, and `1/d` is far below that, because
    /// inverse distance spends most of its fall in the first few blocks.
    #[test]
    fn the_rolloff_is_inverse_distance_and_not_a_linear_ramp() {
        let midpoint = (FULL_GAIN_BLOCKS + RANGE) * 0.5;
        let gain = attenuation(midpoint, RANGE);
        assert!(
            gain < 0.25,
            "a {midpoint}-block gain of {gain} is a linear ramp, not an inverse-distance one"
        );
        // Convex, checked rather than asserted in prose: the first block of walking away
        // costs more than the last one before the range.
        let near =
            attenuation(FULL_GAIN_BLOCKS, RANGE) - attenuation(FULL_GAIN_BLOCKS + 1.0, RANGE);
        let far = attenuation(RANGE - 2.0, RANGE) - attenuation(RANGE - 1.0, RANGE);
        assert!(near > far, "near step {near} should exceed far step {far}");
    }

    /// The doc on [`attenuation`] names four gains. They are here so the sentence cannot go
    /// stale without a test going red — this repository has been caught twice by a comment
    /// that outlived the code it described.
    #[test]
    fn the_curve_is_where_its_documentation_says_it_is() {
        for (distance, expected) in [(4.0, 0.4667), (8.0, 0.2000), (16.0, 0.0667)] {
            let gain = attenuation(distance, RANGE);
            assert!(
                (gain - expected).abs() < 5e-4,
                "{gain} at {distance} blocks, documented as {expected}"
            );
        }
    }

    #[test]
    fn attenuation_never_rises_with_distance() {
        let mut previous = f32::INFINITY;
        for step in 0..=400 {
            let distance = step as f32 * 0.1;
            let gain = attenuation(distance, RANGE);
            assert!((0.0..=1.0).contains(&gain), "{gain} at {distance}");
            assert!(
                gain <= previous,
                "{gain} at {distance} rose from {previous}"
            );
            previous = gain;
        }
    }

    #[test]
    fn a_server_that_relays_no_voice_has_no_gain_at_any_distance() {
        assert_eq!(attenuation(0.0, 0.0), 0.0);
        assert_eq!(attenuation(1.0, 0.0), 0.0);
        assert_eq!(attenuation(1.0, -5.0), 0.0);
    }

    #[test]
    fn rubbish_is_refused_rather_than_clamped() {
        assert_eq!(attenuation(f32::NAN, RANGE), 0.0);
        assert_eq!(attenuation(1.0, f32::NAN), 0.0);
        assert_eq!(attenuation(f32::INFINITY, RANGE), 0.0);
        assert_eq!(azimuth(Vec3::ZERO, f32::NAN, Vec3::X), 0.0);
        assert_eq!(azimuth(Vec3::ZERO, 0.0, Vec3::splat(f32::NAN)), 0.0);
        assert_eq!(pan_gains(f32::NAN), PanGains::UNPOSITIONED);
        assert_eq!(back_cue(f32::NAN), 1.0);
        assert_eq!(band_gains(f32::NAN), [1.0, 1.0, 1.0]);
        assert_eq!(advance(0.5, f32::NAN, 1.0), 0.5);
        assert_eq!(advance(0.5, 1.0, f32::NAN), 0.5);
    }

    /// With the camera at yaw 0 it faces Bevy's `-Z`. The four cardinal answers are what
    /// pin the sign convention, and a swapped `atan2` or a flipped `right` vector fails
    /// three of them.
    #[test]
    fn the_azimuth_is_zero_ahead_and_positive_to_the_right() {
        let listener = Vec3::new(10.0, 64.0, 10.0);
        let ahead = azimuth(listener, 0.0, listener + Vec3::new(0.0, 0.0, -5.0));
        let right = azimuth(listener, 0.0, listener + Vec3::new(5.0, 0.0, 0.0));
        let left = azimuth(listener, 0.0, listener + Vec3::new(-5.0, 0.0, 0.0));
        let behind = azimuth(listener, 0.0, listener + Vec3::new(0.0, 0.0, 5.0));
        assert!(ahead.abs() < 1e-5, "{ahead}");
        assert!((right - FRAC_PI_2).abs() < 1e-5, "{right}");
        assert!((left + FRAC_PI_2).abs() < 1e-5, "{left}");
        assert!((behind.abs() - PI).abs() < 1e-5, "{behind}");
    }

    /// Turning the listener has to move the world the other way. Yaw `π/2` rotates the
    /// forward vector from `-Z` onto `-X`, so a speaker due north is then on the listener's
    /// right — an assertion a sign error in either basis vector fails.
    #[test]
    fn turning_the_listener_moves_the_speaker_around_them() {
        let listener = Vec3::new(0.0, 0.0, 0.0);
        let north = Vec3::new(0.0, 0.0, -5.0);
        assert!(azimuth(listener, 0.0, north).abs() < 1e-5);
        let turned = azimuth(listener, FRAC_PI_2, north);
        assert!((turned - FRAC_PI_2).abs() < 1e-5, "{turned}");
        let turned_back = azimuth(listener, -FRAC_PI_2, north);
        assert!((turned_back + FRAC_PI_2).abs() < 1e-5, "{turned_back}");
    }

    #[test]
    fn height_does_not_enter_the_azimuth() {
        let listener = Vec3::new(0.0, 64.0, 0.0);
        let level = azimuth(listener, 0.0, Vec3::new(5.0, 64.0, 0.0));
        let overhead = azimuth(listener, 0.0, Vec3::new(5.0, 128.0, 0.0));
        assert_eq!(level, overhead);
        // Directly above has no horizontal direction at all, and is placed dead ahead.
        assert_eq!(azimuth(listener, 0.0, Vec3::new(0.0, 128.0, 0.0)), 0.0);
    }

    #[test]
    fn the_pan_law_holds_its_power_all_the_way_round() {
        for step in 0..=360 {
            let azimuth = (step as f32).to_radians() - PI;
            let gains = pan_gains(azimuth);
            let power = gains.left * gains.left + gains.right * gains.right;
            assert!((power - 1.0).abs() < 1e-5, "power {power} at {azimuth}");
            assert!(gains.left >= 0.0 && gains.right >= 0.0);
        }
    }

    /// The assertion that fails if the two channels are swapped, which is the one mistake a
    /// power-constancy test cannot see: `left² + right²` is symmetric in the pair.
    #[test]
    fn a_speaker_on_the_right_is_louder_in_the_right_channel() {
        let right = pan_gains(FRAC_PI_2);
        assert!(right.right > 0.99, "{right:?}");
        assert!(right.left < 0.01, "{right:?}");

        let left = pan_gains(-FRAC_PI_2);
        assert!(left.left > 0.99, "{left:?}");
        assert!(left.right < 0.01, "{left:?}");

        // And the field moves continuously between them rather than switching sides.
        let slightly_right = pan_gains(0.3);
        assert!(slightly_right.right > slightly_right.left);
        assert!(slightly_right.left > 0.5, "{slightly_right:?}");
    }

    /// The pan law is deliberately blind to front and back, and this pins that so the
    /// front/back cue cannot be quietly deleted as redundant.
    #[test]
    fn the_pan_law_cannot_tell_ahead_from_behind() {
        let ahead = pan_gains(0.0);
        let behind = pan_gains(PI);
        assert!((ahead.left - behind.left).abs() < 1e-6);
        assert!((ahead.right - behind.right).abs() < 1e-6);
        assert!((ahead.left - ahead.right).abs() < 1e-6);
    }

    #[test]
    fn the_front_back_cue_takes_the_top_off_a_voice_behind_the_listener() {
        assert_eq!(back_cue(0.0), 1.0);
        assert!((back_cue(PI) - (1.0 - BACK_HIGH_CUT)).abs() < 1e-6);
        // Sides sit halfway, and the two of them agree — a head is symmetric.
        let right = back_cue(FRAC_PI_2);
        let left = back_cue(-FRAC_PI_2);
        assert!((right - left).abs() < 1e-6, "{right} vs {left}");
        assert!(
            (right - (1.0 - BACK_HIGH_CUT * 0.5)).abs() < 1e-6,
            "{right}"
        );
    }

    #[test]
    fn the_front_back_cue_never_goes_further_than_a_wall_would() {
        for step in 0..=360 {
            let angle = (step as f32).to_radians() - PI;
            let cue = back_cue(angle);
            assert!(
                (1.0 - BACK_HIGH_CUT..=1.0).contains(&cue),
                "{cue} at {angle}"
            );
        }
    }

    #[test]
    fn open_air_leaves_every_band_untouched() {
        assert_eq!(band_gains(0.0), [1.0, 1.0, 1.0]);
    }

    /// The discriminating one: a filter whose bands all moved together would pass a
    /// "gains go down" test while being nothing but a volume control.
    #[test]
    fn a_wall_takes_the_high_band_hardest_and_the_low_band_least() {
        let walled = band_gains(1.0);
        for (got, want) in walled
            .iter()
            .zip([OCCLUDED_LOW, OCCLUDED_MID, OCCLUDED_HIGH])
        {
            assert!((got - want).abs() < 1e-6, "{walled:?}");
        }
        assert!(walled[0] > walled[1], "{walled:?}");
        assert!(walled[1] > walled[2], "{walled:?}");

        // And the ordering holds everywhere in between, not only at the ends.
        for step in 1..=100 {
            let occlusion = step as f32 / 100.0;
            let gains = band_gains(occlusion);
            assert!(gains[0] > gains[1], "{gains:?} at {occlusion}");
            assert!(gains[1] > gains[2], "{gains:?} at {occlusion}");
        }
    }

    #[test]
    fn every_band_falls_as_occlusion_rises() {
        let mut previous = [f32::INFINITY; BANDS];
        for step in 0..=100 {
            let occlusion = step as f32 / 100.0;
            let gains = band_gains(occlusion);
            for band in 0..BANDS {
                assert!((0.0..=1.0).contains(&gains[band]));
                assert!(gains[band] <= previous[band], "{gains:?} at {occlusion}");
            }
            previous = gains;
        }
        // Out of range is clamped in rather than extrapolated: an occlusion of 2 is a
        // caller's arithmetic error and must not invert a gain.
        assert_eq!(band_gains(2.0), band_gains(1.0));
        assert_eq!(band_gains(-1.0), band_gains(0.0));
    }

    /// A 10 ms block is roughly what a 48 kHz device asks for at a 512-frame buffer.
    const BLOCK: f32 = 0.010;

    /// A one-pole exponential would still be short of the target here, at every block
    /// count, forever — which is what this equality is for.
    #[test]
    fn the_smoothed_value_reaches_a_wall_in_exactly_fifty_milliseconds() {
        let blocks = (OCCLUSION_ATTACK_SECONDS / BLOCK).round() as usize;
        assert_eq!(blocks, 5);
        let mut value = 0.0;
        for _ in 0..blocks - 1 {
            value = advance(value, 1.0, BLOCK);
            assert!(value < 1.0, "arrived early at {value}");
        }
        value = advance(value, 1.0, BLOCK);
        assert!((value - 1.0).abs() < 1e-5, "{value}");
    }

    #[test]
    fn it_comes_back_out_six_times_more_slowly() {
        let blocks = (OCCLUSION_RELEASE_SECONDS / BLOCK).round() as usize;
        assert_eq!(blocks, 30);
        let mut value = 1.0;
        for _ in 0..blocks - 1 {
            value = advance(value, 0.0, BLOCK);
            assert!(value > 0.0, "arrived early at {value}");
        }
        value = advance(value, 0.0, BLOCK);
        assert!(value.abs() < 1e-5, "{value}");
    }

    #[test]
    fn a_long_block_lands_on_the_target_rather_than_past_it() {
        assert_eq!(advance(0.0, 1.0, 10.0), 1.0);
        assert_eq!(advance(1.0, 0.0, 10.0), 0.0);
        assert_eq!(advance(0.4, 0.4, 10.0), 0.4);
        // A block of no time at all moves nothing.
        assert_eq!(advance(0.25, 1.0, 0.0), 0.25);
        assert_eq!(advance(0.25, 1.0, -1.0), 0.25);
    }

    /// The asymmetry is the whole mechanism, so it gets an assertion of its own: a value
    /// that used the release time to rise would not flicker either, and would take six
    /// times too long to duck behind a pillar.
    #[test]
    fn rising_is_faster_than_falling() {
        let up = advance(0.5, 1.0, BLOCK) - 0.5;
        let down = 0.5 - advance(0.5, 0.0, BLOCK);
        assert!(up > down, "attack {up} should exceed release {down}");
        assert!(
            (up / down - OCCLUSION_RELEASE_SECONDS / OCCLUSION_ATTACK_SECONDS).abs() < 1e-4,
            "{up} vs {down}"
        );
    }

    #[test]
    fn the_crossovers_are_ordered_and_inside_the_unit_interval() {
        let low = one_pole_coefficient(LOW_CROSSOVER_HZ, 48_000);
        let high = one_pole_coefficient(HIGH_CROSSOVER_HZ, 48_000);
        assert!(low > 0.0 && low < high, "{low} vs {high}");
        assert!(high < 1.0, "{high}");
        // Monotonic in frequency, which is what makes the band split a split.
        let mut previous = 0.0;
        for hz in 0..=20_000 {
            let coefficient = one_pole_coefficient(hz as f32, 48_000);
            assert!(coefficient >= previous, "{coefficient} at {hz}Hz");
            assert!((0.0..=1.0).contains(&coefficient));
            previous = coefficient;
        }
    }

    #[test]
    fn a_slower_device_needs_a_larger_coefficient_for_the_same_corner() {
        let fast = one_pole_coefficient(LOW_CROSSOVER_HZ, 48_000);
        let slow = one_pole_coefficient(LOW_CROSSOVER_HZ, 16_000);
        assert!(slow > fast, "{slow} vs {fast}");
    }

    #[test]
    fn a_rate_nobody_can_filter_at_answers_a_wire() {
        assert_eq!(one_pole_coefficient(LOW_CROSSOVER_HZ, 0), 1.0);
        assert_eq!(one_pole_coefficient(f32::NAN, 48_000), 1.0);
        assert_eq!(one_pole_coefficient(0.0, 48_000), 0.0);
    }
}
