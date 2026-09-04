//! libopus, wrapped so the rest of this client sees frames of samples and frames of bytes.
//!
//! **The only place this client calls a codec**, and the seam that keeps `audiopus` out of
//! everything else: `audio/voice.rs` hands it 20 ms of mono and receives bytes, or hands it
//! bytes and receives 20 ms of mono. Nothing above here knows what an Opus packet is, and the
//! one thing below here is a C library.
//!
//! ## What is configured, and why each number
//!
//! - **20 ms frames at 48 kHz mono.** [`dsp::FRAME_SAMPLES`] is the one definition; a call
//!   with any other length is refused rather than encoded at a size the far side is not
//!   expecting.
//! - **24 kbit/s.** Comfortably above what wideband speech needs and far below the ceiling
//!   the contract states, which is the headroom the packet buffer below relies on.
//! - **In-band forward error correction, for 10% expected loss.** Opus can carry a
//!   lower-bitrate copy of the *previous* frame inside the current one. The cost is a few
//!   bits per frame; the return is that one lost packet is recoverable from the next, which
//!   over a proximity chat on an ordinary connection is the difference between a gap and a
//!   word. Telling the encoder how much loss to expect is what makes it spend those bits —
//!   an encoder told nothing writes redundancy nobody asked for or none at all.
//! - **`Application::Voip`**, not `Audio`: the mode that prioritises intelligibility of
//!   speech over fidelity of everything else, which is the whole content of this stream.
//!
//! ## Two ways a frame arrives missing, and they are not the same repair
//!
//! - **Packet-loss concealment** is what the decoder does when told there is *no* packet: it
//!   extrapolates from what it has, so a gap fades rather than clicking. It knows nothing
//!   about the frame that was lost.
//! - **Forward error correction** is what it does when handed the packet *after* the lost
//!   one and told to look inside it: the redundant copy is the real frame, at a lower
//!   bitrate. It is only available while the next packet has already arrived, which is why
//!   this is a jitter buffer's decision (#852 part 7) and not this module's.
//!
//! Both are one call with different arguments, which is why they are one method here with a
//! named enum rather than a `bool` at the call site.
//!
//! ## Nothing here writes a frame down
//!
//! A voice frame is personal data — `schemas/player.fbs` states it as a constraint on every
//! consumer of the table, and this is one. No error path in this file quotes a payload; the
//! diagnostics name lengths and nothing else.

// The consumers are `audio/voice.rs`'s capture pipeline in #852 part 6 and its playback half
// in part 7. In a binary crate `pub` saves nothing from `dead_code`, and the alternative is a
// seam that puts the codec, the capture device and the pipeline over both in one pull
// request. Part 6 removes the encoder's half of this and part 7 the decoder's.
#![allow(dead_code)]

use audiopus::coder::{Decoder, Encoder};
use audiopus::packet::Packet;
use audiopus::{Application, Bitrate, Channels, MutSignals, SampleRate};

use super::dsp::FRAME_SAMPLES;
use crate::net::MAX_OPUS_BYTES;

/// What this client encodes at, in bits per second.
///
/// Wideband speech is intelligible from about 16 kbit/s and transparent for this purpose
/// well before 32. 24 leaves room for the redundancy [`EXPECTED_LOSS_PERCENT`] asks for
/// without spending bandwidth a voxel world needs for chunks.
pub const VOICE_BITRATE: i32 = 24_000;

/// How much packet loss the encoder is told to expect, as a percentage.
///
/// **It is a request for redundancy, not a measurement.** Opus spends bits on the in-band
/// copy in proportion to this, so zero means no forward error correction however enabled it
/// is. Ten percent is a pessimistic figure for a domestic connection and a cheap one: the
/// difference at 24 kbit/s is a few bytes a frame.
pub const EXPECTED_LOSS_PERCENT: u8 = 10;

/// Which repair the decoder should attempt for a frame it was not given.
///
/// A named enum rather than the `bool` libopus takes, because at the call site `true` and
/// `false` say nothing about which of two different mechanisms is being asked for — and the
/// two need different inputs, which is the part a `bool` hides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// Nothing arrived and nothing later has either. The decoder extrapolates from what it
    /// already played, so the gap fades instead of clicking.
    Conceal,
    /// The frame *after* the lost one has arrived and is being handed over for the redundant
    /// copy inside it. What comes out is the lost frame, at a lower bitrate — a real repair
    /// rather than a plausible noise.
    FromTheNext,
}

/// One speaker's encoder: 20 ms of mono in, an Opus packet out.
///
/// **The packet buffer is [`MAX_OPUS_BYTES`] and that is the ceiling check.** `net/codec.rs`
/// deliberately does not enforce the contract's limit — a refusal there could only drop a
/// frame — so it is enforced here, where the thing that decides a frame's length lives.
/// libopus refuses to write past the buffer it is given, so an encoder configured to produce
/// more than the contract allows fails loudly at the first frame rather than sending packets
/// a relay silently drops.
#[derive(Debug)]
pub struct VoiceEncoder {
    encoder: Encoder,
    /// Reused across frames, so encoding fifty times a second allocates nothing.
    packet: Vec<u8>,
}

impl VoiceEncoder {
    /// An encoder configured as this module's doc describes, or the reason there is none.
    ///
    /// **A `Result` and never a panic.** libopus is a C library reached through a build-time
    /// link; a client that could not configure it plays a silent game with a line in the log,
    /// exactly as one with no sound card does.
    pub fn new() -> Result<Self, String> {
        let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)
            .map_err(|err| format!("cannot open the voice encoder: {err}"))?;
        encoder
            .set_bitrate(Bitrate::BitsPerSecond(VOICE_BITRATE))
            .map_err(|err| format!("cannot set the voice bitrate: {err}"))?;
        encoder
            .set_inband_fec(true)
            .map_err(|err| format!("cannot enable forward error correction: {err}"))?;
        // After `set_inband_fec`, and load-bearing: the loss figure is what makes the
        // encoder actually spend bits on the redundant copy. Enabled and told to expect no
        // loss, it writes none.
        encoder
            .set_packet_loss_perc(EXPECTED_LOSS_PERCENT)
            .map_err(|err| format!("cannot set the expected packet loss: {err}"))?;
        Ok(Self {
            encoder,
            packet: vec![0; MAX_OPUS_BYTES],
        })
    }

    /// Encodes exactly [`FRAME_SAMPLES`] of mono, and borrows the packet back.
    ///
    /// Borrowed rather than owned so a frame every 20 ms is not an allocation every 20 ms;
    /// the caller copies it into the wire frame it is building, which is a copy it makes
    /// anyway.
    pub fn encode(&mut self, frame: &[f32]) -> Result<&[u8], String> {
        if frame.len() != FRAME_SAMPLES {
            // The length and never the samples: see the module doc.
            return Err(format!(
                "a voice frame is {FRAME_SAMPLES} samples, not {}",
                frame.len()
            ));
        }
        let written = self
            .encoder
            .encode_float(frame, &mut self.packet)
            .map_err(|err| format!("cannot encode a voice frame: {err}"))?;
        Ok(&self.packet[..written])
    }

    /// What the encoder is actually running at, read back from libopus rather than from the
    /// constant that asked for it. The tests below are the only caller: a setting this module
    /// believes it applied and did not is exactly the failure a re-read catches.
    #[cfg(test)]
    fn configured(&self) -> (Bitrate, bool, u8) {
        (
            self.encoder.bitrate().expect("libopus reports its bitrate"),
            self.encoder.inband_fec().expect("libopus reports its fec"),
            self.encoder
                .packet_loss_perc()
                .expect("libopus reports its loss figure"),
        )
    }
}

/// One speaker's decoder: an Opus packet in, 20 ms of mono out.
///
/// **One per speaker, and that is not an optimisation.** An Opus decoder carries the state
/// its concealment extrapolates from, so two speakers sharing one would each be concealed
/// from the other's audio. `audio/voice.rs` keeps one beside each jitter buffer.
#[derive(Debug)]
pub struct VoiceDecoder {
    decoder: Decoder,
}

impl VoiceDecoder {
    /// A decoder at [`VOICE_SAMPLE_RATE`], mono, or the reason there is none.
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            decoder: Decoder::new(SampleRate::Hz48000, Channels::Mono)
                .map_err(|err| format!("cannot open a voice decoder: {err}"))?,
        })
    }

    /// Decodes one packet into `out`, which must hold [`FRAME_SAMPLES`], and answers how many
    /// samples were written.
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<usize, String> {
        if out.len() < FRAME_SAMPLES {
            return Err(format!(
                "a decode buffer is {FRAME_SAMPLES} samples, not {}",
                out.len()
            ));
        }
        let packet = Packet::try_from(packet)
            .map_err(|err| format!("a voice packet was refused before decoding: {err}"))?;
        let signals = MutSignals::try_from(&mut out[..FRAME_SAMPLES])
            .map_err(|err| format!("a decode buffer was refused: {err}"))?;
        self.decoder
            .decode_float(Some(packet), signals, false)
            .map_err(|err| format!("cannot decode a voice frame: {err}"))
    }

    /// Produces one frame for a packet that never arrived.
    ///
    /// [`Missing::Conceal`] extrapolates and takes no packet. [`Missing::FromTheNext`] takes
    /// the packet that arrived *after* the lost one and reads the redundant copy out of it —
    /// so `next` is required there and ignored for concealment, which is why the two are one
    /// method: the argument that differs is the one the enum names.
    pub fn repair(
        &mut self,
        how: Missing,
        next: Option<&[u8]>,
        out: &mut [f32],
    ) -> Result<usize, String> {
        if out.len() < FRAME_SAMPLES {
            return Err(format!(
                "a decode buffer is {FRAME_SAMPLES} samples, not {}",
                out.len()
            ));
        }
        let packet = match (how, next) {
            (Missing::FromTheNext, Some(bytes)) => Some(
                Packet::try_from(bytes)
                    .map_err(|err| format!("a voice packet was refused before decoding: {err}"))?,
            ),
            // Concealment reads nothing, and a repair that was asked to read the next packet
            // without being given one is concealment: the caller lost the race between the
            // deadline and the arrival, and a gap that fades beats an error nobody can act on.
            _ => None,
        };
        let fec = packet.is_some();
        let signals = MutSignals::try_from(&mut out[..FRAME_SAMPLES])
            .map_err(|err| format!("a decode buffer was refused: {err}"))?;
        self.decoder
            .decode_float(packet, signals, fec)
            .map_err(|err| format!("cannot conceal a lost voice frame: {err}"))
    }
}

/// **No test here opens a device.** libopus is a pure function of its input: everything below
/// runs the real encoder and the real decoder in this process, over generated samples, which
/// is what the issue's test strategy asks for and what makes the round trip meaningful rather
/// than a mock agreeing with itself.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::{VOICE_SAMPLE_RATE, level_db};

    /// One 20 ms frame of a sine at `hz`, at `amplitude`.
    fn frame(hz: f32, amplitude: f32, at: usize) -> Vec<f32> {
        (0..FRAME_SAMPLES)
            .map(|index| {
                let sample = (at * FRAME_SAMPLES + index) as f32;
                let phase = std::f32::consts::TAU * hz * sample / VOICE_SAMPLE_RATE as f32;
                amplitude * phase.sin()
            })
            .collect()
    }

    /// The configuration this module claims, read back out of libopus rather than out of the
    /// constants that asked for it.
    ///
    /// **A setting believed and not applied is the failure this catches**, and it is not
    /// hypothetical here: `set_packet_loss_perc` is what makes `set_inband_fec` spend any
    /// bits at all, so an encoder with the flag on and the figure at zero writes no
    /// redundancy while every name in the source says it does.
    #[test]
    fn the_encoder_runs_at_the_settings_this_module_asks_for() {
        let encoder = VoiceEncoder::new().expect("libopus is linked");
        let (bitrate, fec, loss) = encoder.configured();
        assert_eq!(bitrate, Bitrate::BitsPerSecond(VOICE_BITRATE));
        assert!(fec, "forward error correction is off");
        assert_eq!(loss, EXPECTED_LOSS_PERCENT);
    }

    /// The round trip: 20 ms in, a packet well under the contract's ceiling, 20 ms back, and
    /// the energy of the signal preserved.
    ///
    /// Opus is lossy, so this asserts the *level* rather than the samples — a codec that
    /// returned silence, noise or a signal 20 dB down would fail, and one that reproduced the
    /// tone imperfectly is doing its job.
    #[test]
    fn a_frame_survives_the_round_trip_with_its_energy() {
        let mut encoder = VoiceEncoder::new().expect("libopus is linked");
        let mut decoder = VoiceDecoder::new().expect("libopus is linked");
        let mut out = vec![0.0f32; FRAME_SAMPLES];

        // The first frames of any Opus stream are the codec settling; what is asserted is a
        // steady state, which is what a conversation is made of.
        let mut heard = Vec::new();
        for at in 0..20 {
            let sent = frame(300.0, 0.4, at);
            let packet = encoder.encode(&sent).expect("the frame encodes").to_vec();
            assert!(!packet.is_empty(), "an empty packet is not a frame");
            assert!(
                packet.len() <= MAX_OPUS_BYTES,
                "a {VOICE_BITRATE} bit/s frame is {} bytes, over the contract's ceiling",
                packet.len()
            );
            assert_eq!(
                decoder
                    .decode(&packet, &mut out)
                    .expect("the packet decodes"),
                FRAME_SAMPLES
            );
            heard.push(out.clone());
        }

        let sent = level_db(&frame(300.0, 0.4, 19));
        let played = level_db(&heard[19]);
        assert!(
            (sent - played).abs() < 3.0,
            "0.4 amplitude went in at {sent} dB and came out at {played} dB"
        );
    }

    /// At 24 kbit/s a 20 ms frame is about sixty bytes, which is the headroom the ceiling
    /// check in `net/codec.rs` is documented as relying on. Asserted as a band rather than a
    /// number: the codec is free to vary, and what matters is the order of magnitude.
    #[test]
    fn a_frame_at_this_bitrate_is_a_long_way_under_the_contracts_ceiling() {
        let mut encoder = VoiceEncoder::new().expect("libopus is linked");
        let mut largest = 0;
        for at in 0..50 {
            let packet = encoder
                .encode(&frame(220.0, 0.9, at))
                .expect("the frame encodes");
            largest = largest.max(packet.len());
        }
        assert!(
            (20..=MAX_OPUS_BYTES / 2).contains(&largest),
            "the largest frame of fifty was {largest} bytes"
        );
    }

    /// **Concealment is not silence, and it is not a click either.** A decoder handed nothing
    /// extrapolates from what it played last, so what comes out has energy and joins onto the
    /// previous frame without a step.
    #[test]
    fn a_concealed_frame_is_neither_silence_nor_a_click() {
        let mut encoder = VoiceEncoder::new().expect("libopus is linked");
        let mut decoder = VoiceDecoder::new().expect("libopus is linked");
        let mut out = vec![0.0f32; FRAME_SAMPLES];

        let mut last = 0.0;
        for at in 0..20 {
            let packet = encoder
                .encode(&frame(300.0, 0.4, at))
                .expect("the frame encodes")
                .to_vec();
            decoder.decode(&packet, &mut out).expect("it decodes");
            last = out[FRAME_SAMPLES - 1];
        }

        let mut concealed = vec![0.0f32; FRAME_SAMPLES];
        assert_eq!(
            decoder
                .repair(Missing::Conceal, None, &mut concealed)
                .expect("concealment produces a frame"),
            FRAME_SAMPLES
        );
        assert!(
            level_db(&concealed) > -40.0,
            "concealment produced silence at {} dB",
            level_db(&concealed)
        );
        assert!(
            (concealed[0] - last).abs() < 0.25,
            "concealment started with a step of {} from the previous frame",
            (concealed[0] - last).abs()
        );
        assert!(
            concealed.iter().all(|sample| sample.is_finite()),
            "concealment produced something that is not a number"
        );
    }

    /// **The forward error correction, doing the thing it costs bits for.** One frame is
    /// dropped and recovered out of the *next* packet, and what comes back is that frame's
    /// own audio rather than a plausible continuation of the frame before it.
    ///
    /// **The signal is chosen so the two answers can differ, which took a failing test to
    /// learn.** On a steady tone, concealment is nearly perfect — extrapolating a periodic
    /// waveform is exactly what it is good at — so the first version of this compared the
    /// two on a constant sine and the *concealed* frame won. What separates them is a signal
    /// that changes across the gap: ten quiet frames, then a loud one that never arrives.
    /// Concealment can only carry on being quiet; the redundant copy inside the next packet
    /// knows the frame was loud.
    ///
    /// Levels rather than samples, deliberately: Opus is lossy and carries its own delay, so
    /// a sample-by-sample comparison would be measuring alignment rather than repair.
    #[test]
    fn a_lost_frame_is_recovered_from_the_packet_after_it() {
        let quiet = 0.02;
        let loud = 0.6;
        let mut encoder = VoiceEncoder::new().expect("libopus is linked");
        let mut packets = Vec::new();
        let mut sent = Vec::new();
        for at in 0..12 {
            let block = frame(300.0, if at >= 10 { loud } else { quiet }, at);
            packets.push(encoder.encode(&block).expect("the frame encodes").to_vec());
            sent.push(block);
        }

        // Two decoders fed the identical first ten frames, so what differs between them is
        // only how each answers the eleventh's absence.
        let mut recovering = VoiceDecoder::new().expect("libopus is linked");
        let mut concealing = VoiceDecoder::new().expect("libopus is linked");
        let mut scratch = vec![0.0f32; FRAME_SAMPLES];
        for packet in packets.iter().take(10) {
            recovering.decode(packet, &mut scratch).expect("it decodes");
            concealing.decode(packet, &mut scratch).expect("it decodes");
        }

        // Frame 10 never arrives. One decoder is handed frame 11 and asked for the redundant
        // copy; the other is told there is nothing.
        let mut recovered = vec![0.0f32; FRAME_SAMPLES];
        recovering
            .repair(Missing::FromTheNext, Some(&packets[11]), &mut recovered)
            .expect("the redundant copy decodes");
        let mut concealed = vec![0.0f32; FRAME_SAMPLES];
        concealing
            .repair(Missing::Conceal, None, &mut concealed)
            .expect("concealment produces a frame");

        let lost = level_db(&sent[10]);
        let repaired = level_db(&recovered);
        let guessed = level_db(&concealed);
        assert!(
            repaired > guessed + 6.0,
            "the recovered frame ({repaired} dB) is no louder than the concealed one \
             ({guessed} dB), for a lost frame that was {lost} dB"
        );
        assert!(
            (repaired - lost).abs() < 6.0,
            "the recovered frame is {repaired} dB where the lost one was {lost} dB"
        );
        assert!(
            recovered.iter().all(|sample| sample.is_finite()),
            "the recovery produced something that is not a number"
        );
    }

    /// **A repair asked to read the next packet without one is concealment, not an error.**
    /// The caller lost the race between its deadline and the arrival; a gap that fades beats
    /// a failure nobody can act on.
    #[test]
    fn a_recovery_with_no_next_packet_conceals_instead_of_failing() {
        let mut encoder = VoiceEncoder::new().expect("libopus is linked");
        let mut decoder = VoiceDecoder::new().expect("libopus is linked");
        let mut out = vec![0.0f32; FRAME_SAMPLES];
        for at in 0..5 {
            let packet = encoder
                .encode(&frame(300.0, 0.4, at))
                .expect("the frame encodes")
                .to_vec();
            decoder.decode(&packet, &mut out).expect("it decodes");
        }
        assert_eq!(
            decoder
                .repair(Missing::FromTheNext, None, &mut out)
                .expect("it conceals rather than failing"),
            FRAME_SAMPLES
        );
    }

    /// Wrong-sized buffers are refused rather than encoded or decoded at a length the far
    /// side is not expecting — and the refusals name lengths and never samples or bytes,
    /// which is the personal-data rule this module is a consumer of.
    #[test]
    fn a_wrongly_sized_frame_is_refused_and_no_refusal_quotes_a_payload() {
        let mut encoder = VoiceEncoder::new().expect("libopus is linked");
        let mut decoder = VoiceDecoder::new().expect("libopus is linked");

        for length in [0, FRAME_SAMPLES - 1, FRAME_SAMPLES + 1] {
            let refusal = encoder
                .encode(&vec![0.1; length])
                .expect_err("a frame of the wrong length was encoded");
            assert!(refusal.contains(&FRAME_SAMPLES.to_string()), "{refusal}");
            assert!(
                !refusal.contains("0.1"),
                "a refusal quoted a sample: {refusal}"
            );
        }

        let mut short = vec![0.0f32; FRAME_SAMPLES - 1];
        assert!(decoder.decode(&[0x78], &mut short).is_err());
        assert!(decoder.repair(Missing::Conceal, None, &mut short).is_err());

        // And a packet that is not one: refused, with nothing of it in the message.
        let mut out = vec![0.0f32; FRAME_SAMPLES];
        let refusal = decoder
            .decode(&[], &mut out)
            .expect_err("an empty packet decoded");
        assert!(!refusal.is_empty());
    }
}
