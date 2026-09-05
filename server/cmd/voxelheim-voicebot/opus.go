package main

import (
	"encoding/binary"
	"fmt"
	"time"
)

// The frame every synthetic speaker sends, and the one place this command decides what
// "twenty milliseconds of silence" is on the wire.
//
// **There is no Opus dependency here and there is none on the server.** The relay copies
// `opus` verbatim and never parses it — `schemas/player.fbs` says so and
// `internal/game/voice.go` is written to it — so a soak test could have sent any bytes at
// all. It sends a real packet anyway, because the fixture that is only valid by accident
// is the one a later reader trusts for something it cannot do: this is the frame the
// interop step hands to a client with a decoder in it.
//
// The construction is RFC 6716 §3.2, code 3, and every byte of it is in the specification
// rather than in a captured recording:
//
//   - The TOC byte names a configuration and a frame-packing code. [opusConfigSilkWide20ms]
//     is SILK wideband at the 20 ms frame size the client's encoder uses, mono, and code 3
//     is the arbitrary-frame-count packing — the only one that admits padding.
//   - The frame-count byte says CBR (v=0), padding present (p=1) and one frame (M=1).
//   - One frame of length zero. RFC 6716 §3.2.1 makes a zero-length frame legal and gives
//     it a meaning a decoder already has: it is what an encoder emits under discontinuous
//     transmission, which is exactly what a silent speaker is.
//   - Everything after that is padding, which §3.2.5 requires a decoder to ignore.
//
// **The padding is what carries the measurement**, and that is the reason this file exists
// rather than a four-byte literal. A relay latency is the difference between two clocks,
// and the only way to subtract them without a bookkeeping table shared by every bot in the
// process is to put the send instant *inside the frame* — the server copies it across for
// free. Ignored bytes at the end of a legal packet are the one place in this contract where
// a sender may write something the receiver's decoder will not choke on.
const (
	// opusSilenceHeaderBytes is the TOC byte and the frame-count byte, which every packet
	// this file builds starts with. What follows them is the padding length and the padding.
	opusSilenceHeaderBytes = 2

	// opusConfigSilkWide20ms is TOC configuration 9: SILK, wideband, 20 ms.
	opusConfigSilkWide20ms = 9

	// opusCodeArbitrary is TOC frame-packing code 3, the only code that admits padding.
	opusCodeArbitrary = 3

	// opusFrameCountCBRPadded is the frame-count byte: v=0 (every frame the same size),
	// p=1 (padding follows), M=1 (one frame).
	opusFrameCountCBRPadded = 0x41

	// opusPaddingContinues is the padding-length byte that means "254 bytes, and read
	// another length byte".
	opusPaddingContinues = 255
)

// silenceFrame builds one Opus silence packet of exactly size bytes.
//
// The returned slice is a template: [stampSilenceFrame] overwrites its last
// [opusStampBytes] and nothing else, so one template is built per run and every frame a
// speaker sends is a copy of it with a fresh instant in the tail.
func silenceFrame(size int) ([]byte, error) {
	if size < minSilenceBytes || size > maxSilenceBytes {
		return nil, fmt.Errorf(
			"an opus frame must be %d..%d bytes — %d is below what a timed padding tail needs or above what the contract allows, got %d",
			minSilenceBytes, maxSilenceBytes, size, size)
	}

	packet := make([]byte, 0, size)
	packet = append(packet, byte(opusConfigSilkWide20ms<<3|opusCodeArbitrary), opusFrameCountCBRPadded)

	// One length byte encodes up to 254 bytes of padding and two encode up to 508, which
	// is past the contract's ceiling. Both arms are written so the packet's total length
	// is the number that was asked for: the length bytes are part of it.
	//
	// The arithmetic, once: a packet is 2 header bytes, L length bytes, M×R frame bytes
	// (zero here) and P padding bytes, so P = size − 2 − L.
	switch padding := size - 3; {
	case padding <= 254:
		packet = append(packet, byte(padding))
	default:
		padding = size - 4
		if padding-254 > 254 {
			// Unreachable while maxSilenceBytes is 400. Stated rather than assumed,
			// because the ceiling is read from the schema and the schema can move.
			return nil, fmt.Errorf("an opus frame of %d bytes needs more padding-length bytes than this builder writes", size)
		}
		packet = append(packet, opusPaddingContinues, byte(padding-254))
	}

	// Zero-length frame, then the padding itself. The padding is zeroed rather than left
	// to whatever append gave us: a soak test that shipped uninitialised memory across a
	// wire would be a data leak with a measurement attached to it.
	return append(packet, make([]byte, size-len(packet))...), nil
}

// stampSilenceFrame writes the send instant into the packet's padding tail.
//
// A monotonic-clock reading rather than a wall-clock one is not available here — the value
// has to survive an encode, a relay and a decode as plain bytes — so this is
// [time.Time.UnixNano] and the receiver subtracts the same. Every bot in a run shares one
// process and therefore one clock, which is what makes the subtraction mean anything; a
// bot process on another machine would be measuring clock skew and this command does not
// offer that mode.
func stampSilenceFrame(packet []byte, at time.Time) {
	binary.BigEndian.PutUint64(packet[len(packet)-opusStampBytes:], uint64(at.UnixNano()))
}

// readSilenceStamp reads back what [stampSilenceFrame] wrote, and says whether the frame
// was long enough to have carried one at all.
//
// **A frame this command did not send is not an error here.** The bytes came off a wire
// and a listener has no way to know which speaker built them; a short frame is answered
// with false and counted as unmeasurable rather than fatal.
func readSilenceStamp(opus []byte) (time.Time, bool) {
	if len(opus) < minSilenceBytes {
		return time.Time{}, false
	}
	nanos := binary.BigEndian.Uint64(opus[len(opus)-opusStampBytes:])
	if nanos == 0 {
		return time.Time{}, false
	}
	return time.Unix(0, int64(nanos)), true
}
