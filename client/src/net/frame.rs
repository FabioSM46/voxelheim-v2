//! Length-prefixed framing, mirroring `server/internal/transport/frame.go`.
//!
//! One frame is a `u32` big-endian length prefix followed by exactly that many
//! payload bytes. Nothing here knows what a frame means; that is `codec`'s job,
//! exactly as the server keeps `transport` ignorant of `protocol`.
//!
//! The reader is incremental because TCP is a byte stream: a frame arrives split
//! across reads as readily as two frames arrive in one. `FrameDecoder` owns that
//! reassembly so the socket loop never has to.

use std::fmt;
use std::io::{self, Write};

/// The largest payload one frame may carry.
///
/// Identical to `transport.MaxFrameSize` on the server, and load-bearing for the
/// same reason: the length prefix arrives from the network, so an unchecked
/// prefix is a peer-chosen allocation — four bytes asking for four gigabytes.
/// 1 MiB leaves ample headroom for the largest message the contract can produce
/// while keeping a hostile prefix free to refuse.
pub const MAX_FRAME_SIZE: usize = 1 << 20;

/// Width of the big-endian length prefix.
pub const FRAME_HEADER_SIZE: usize = 4;

/// A frame that is malformed at the framing layer, before any byte of it is
/// interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The frame declared zero bytes of payload. No valid `Envelope` is empty,
    /// so an empty frame is malformed rather than idle.
    Empty,
    /// The declared size exceeds [`MAX_FRAME_SIZE`]. Reported from the prefix
    /// alone — the payload is never waited for and never allocated.
    TooLarge { declared: usize },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "frame is empty"),
            Self::TooLarge { declared } => {
                write!(
                    f,
                    "frame exceeds maximum size: {declared} > {MAX_FRAME_SIZE}"
                )
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Reassembles frames from however TCP happens to deliver them.
///
/// Feed it whatever a read produced; ask for frames until it says there are none
/// left. Bytes that do not yet complete a frame stay buffered.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffered: Vec<u8>,
}

impl FrameDecoder {
    /// A decoder holding no bytes.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds freshly read bytes to the buffer.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buffered.extend_from_slice(bytes);
    }

    /// How many bytes are waiting for the rest of their frame.
    ///
    /// Test-only: it exists so the tests can assert that a refused prefix never
    /// grew the buffer, which is the one property of this type that is invisible
    /// from its return values.
    #[cfg(test)]
    pub fn buffered(&self) -> usize {
        self.buffered.len()
    }

    /// Takes the next complete frame, if one has arrived.
    ///
    /// `Ok(None)` means "not yet" and is the normal case; an `Err` means the
    /// stream can no longer be trusted, because there is no way to resynchronise
    /// a byte stream whose framing has stopped making sense.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        if self.buffered.len() < FRAME_HEADER_SIZE {
            return Ok(None);
        }

        let mut header = [0u8; FRAME_HEADER_SIZE];
        header.copy_from_slice(&self.buffered[..FRAME_HEADER_SIZE]);
        let declared = u32::from_be_bytes(header) as usize;

        // Both checks precede every allocation and every wait below. That
        // ordering is the security property, not an optimisation: a 4 GiB prefix
        // is refused from four bytes, without reserving anything and without
        // blocking for a payload that will never come.
        if declared == 0 {
            return Err(FrameError::Empty);
        }
        if declared > MAX_FRAME_SIZE {
            return Err(FrameError::TooLarge { declared });
        }

        let end = FRAME_HEADER_SIZE + declared;
        if self.buffered.len() < end {
            return Ok(None);
        }

        let frame = self.buffered[FRAME_HEADER_SIZE..end].to_vec();
        self.buffered.drain(..end);
        Ok(Some(frame))
    }
}

/// Wraps a payload in its length prefix.
///
/// Returns one buffer rather than writing a header and a payload separately: the
/// server pairs its two writes with a buffered writer to avoid two packets per
/// frame, and a single `write_all` reaches the same place with less to get wrong.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_FRAME_SIZE {
        return Err(FrameError::TooLarge {
            declared: payload.len(),
        });
    }

    // The length check above is what makes this conversion total: MAX_FRAME_SIZE
    // fits in a u32 with room to spare.
    let length = payload.len() as u32;

    let mut frame = Vec::with_capacity(FRAME_HEADER_SIZE + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Writes a payload as one length-prefixed frame.
pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    let frame = encode_frame(payload).map_err(io::Error::other)?;
    writer.write_all(&frame)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The framing is a contract with the Go server, not a local convention.
    /// If `transport.MaxFrameSize` ever moves, this is the test that should fail
    /// before anything reaches the wire.
    #[test]
    fn limits_match_the_server() {
        assert_eq!(MAX_FRAME_SIZE, 1 << 20);
        assert_eq!(FRAME_HEADER_SIZE, 4);
    }

    #[test]
    fn prefix_is_big_endian() {
        let frame = encode_frame(&[0xAA]).expect("a one-byte payload is a valid frame");
        assert_eq!(frame, vec![0x00, 0x00, 0x00, 0x01, 0xAA]);
    }

    #[test]
    fn round_trips_a_payload() {
        let payload = b"envelope bytes".to_vec();
        let mut decoder = FrameDecoder::new();
        decoder.feed(&encode_frame(&payload).expect("valid payload"));

        assert_eq!(decoder.next_frame(), Ok(Some(payload)));
        assert_eq!(decoder.next_frame(), Ok(None));
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn a_partial_header_yields_nothing() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&[0x00, 0x00, 0x00]);

        assert_eq!(decoder.next_frame(), Ok(None));
        assert_eq!(decoder.buffered(), 3, "the partial header stays buffered");
    }

    #[test]
    fn a_partial_payload_yields_nothing() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&[0x00, 0x00, 0x00, 0x04, 0xDE, 0xAD]);

        assert_eq!(decoder.next_frame(), Ok(None));
        assert_eq!(decoder.buffered(), 6);
    }

    #[test]
    fn a_frame_split_across_two_reads_is_reassembled() {
        let payload = b"split me".to_vec();
        let frame = encode_frame(&payload).expect("valid payload");
        let (first, second) = frame.split_at(5);

        let mut decoder = FrameDecoder::new();
        decoder.feed(first);
        assert_eq!(decoder.next_frame(), Ok(None));

        decoder.feed(second);
        assert_eq!(decoder.next_frame(), Ok(Some(payload)));
    }

    #[test]
    fn a_frame_split_byte_by_byte_is_reassembled() {
        let payload = b"one byte at a time".to_vec();
        let frame = encode_frame(&payload).expect("valid payload");

        let mut decoder = FrameDecoder::new();
        for (index, byte) in frame.iter().enumerate() {
            decoder.feed(&[*byte]);
            let last = index + 1 == frame.len();
            let expected = if last { Some(payload.clone()) } else { None };
            assert_eq!(decoder.next_frame(), Ok(expected), "byte {index}");
        }
    }

    #[test]
    fn two_frames_in_one_read_are_both_returned() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&encode_frame(b"first").expect("valid payload"));
        decoder.feed(&encode_frame(b"second").expect("valid payload"));

        assert_eq!(decoder.next_frame(), Ok(Some(b"first".to_vec())));
        assert_eq!(decoder.next_frame(), Ok(Some(b"second".to_vec())));
        assert_eq!(decoder.next_frame(), Ok(None));
    }

    #[test]
    fn a_zero_length_frame_is_rejected() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&[0x00, 0x00, 0x00, 0x00]);

        assert_eq!(decoder.next_frame(), Err(FrameError::Empty));
    }

    #[test]
    fn an_oversized_prefix_is_rejected_from_the_prefix_alone() {
        let declared = MAX_FRAME_SIZE + 1;
        let mut decoder = FrameDecoder::new();
        // Only the four header bytes are fed. Nothing may wait for, or reserve
        // room for, a payload this size.
        decoder.feed(&(declared as u32).to_be_bytes());

        assert_eq!(decoder.next_frame(), Err(FrameError::TooLarge { declared }));
        assert_eq!(
            decoder.buffered(),
            FRAME_HEADER_SIZE,
            "the rejection must not have grown the buffer"
        );
    }

    #[test]
    fn the_largest_declarable_prefix_is_rejected() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&u32::MAX.to_be_bytes());

        assert_eq!(
            decoder.next_frame(),
            Err(FrameError::TooLarge {
                declared: u32::MAX as usize
            })
        );
    }

    #[test]
    fn a_maximum_sized_frame_is_accepted() {
        let payload = vec![0x5A; MAX_FRAME_SIZE];
        let frame = encode_frame(&payload).expect("exactly the maximum is allowed");

        let mut decoder = FrameDecoder::new();
        decoder.feed(&frame);
        assert_eq!(decoder.next_frame(), Ok(Some(payload)));
    }

    #[test]
    fn encoding_refuses_an_empty_payload() {
        assert_eq!(encode_frame(&[]), Err(FrameError::Empty));
    }

    #[test]
    fn encoding_refuses_an_oversized_payload() {
        let payload = vec![0u8; MAX_FRAME_SIZE + 1];
        assert_eq!(
            encode_frame(&payload),
            Err(FrameError::TooLarge {
                declared: MAX_FRAME_SIZE + 1
            })
        );
    }

    #[test]
    fn writing_reports_an_invalid_payload_as_an_error() {
        let mut sink = Vec::new();
        let err = write_frame(&mut sink, &[]).expect_err("an empty payload is not writable");

        assert!(sink.is_empty(), "nothing may reach the socket");
        assert!(err.to_string().contains("empty"), "got {err}");
    }

    #[test]
    fn writing_produces_bytes_the_decoder_accepts() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"handshake").expect("valid payload");

        let mut decoder = FrameDecoder::new();
        decoder.feed(&wire);
        assert_eq!(decoder.next_frame(), Ok(Some(b"handshake".to_vec())));
    }
}
