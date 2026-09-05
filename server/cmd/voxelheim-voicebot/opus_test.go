package main

import (
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// **The packet is checked against RFC 6716 rather than against itself.** A test that only
// asserted the length would pass on any byte string of the right size, which is exactly the
// fixture this file exists not to be: the server never parses these bytes, so nothing else
// in this repository can notice that they stopped being Opus.

func TestASilenceFrameIsAPaddedOpusPacketAtEverySizeItOffers(t *testing.T) {
	t.Parallel()

	for _, size := range []int{minSilenceBytes, 12, 96, 257, 258, 300, protocol.MaxVoiceOpusBytes} {
		packet, err := silenceFrame(size)
		if err != nil {
			t.Fatalf("silenceFrame(%d): %v", size, err)
		}
		if len(packet) != size {
			t.Fatalf("silenceFrame(%d) is %d bytes", size, len(packet))
		}

		// The TOC: configuration 9 in the top five bits, mono, and code 3 in the bottom two.
		if got := packet[0] >> 3; got != opusConfigSilkWide20ms {
			t.Errorf("size %d: TOC names configuration %d, want %d", size, got, opusConfigSilkWide20ms)
		}
		if got := packet[0] & 0x04; got != 0 {
			t.Errorf("size %d: TOC says stereo", size)
		}
		if got := packet[0] & 0x03; got != opusCodeArbitrary {
			t.Errorf("size %d: TOC names frame-packing code %d, want %d", size, got, opusCodeArbitrary)
		}
		// The frame-count byte, read as the three fields RFC 6716 §3.2.5 lays it out in
		// rather than compared against the constant that defines it. #931's review is why:
		// `packet[1] != opusFrameCountCBRPadded` is a tautology, and it would have passed
		// on any value at all — including one a decoder refuses.
		//
		// **M is the frame count itself, in 1..48, not the count minus one.** That is the
		// claim the tautology was hiding, it is the one the review disputed, and 0x40 —
		// what "minus one" would make of a single-frame packet — is refused by libopus as
		// a corrupted stream rather than read as one frame.
		if vbr := packet[1] & 0x80; vbr != 0 {
			t.Errorf("size %d: the frame-count byte says VBR, and every frame here is the same size", size)
		}
		if padded := packet[1] & 0x40; padded == 0 {
			t.Errorf("size %d: the frame-count byte says there is no padding, and the packet is almost all padding", size)
		}
		if frames := packet[1] & 0x3F; frames != 1 {
			t.Errorf("size %d: the frame-count byte carries M=%d, want the one frame this packet holds "+
				"(M is the count, and 0 is a value the RFC forbids)", size, frames)
		}

		// The padding the length bytes declare has to be exactly the padding that is
		// there, or a decoder reads a frame out of what this file meant as filler.
		declared, lengthBytes := declaredPadding(t, packet)
		if want := size - 2 - lengthBytes; declared != want {
			t.Errorf("size %d: %d length byte(s) declare %d bytes of padding, but %d follow the header",
				size, lengthBytes, declared, want)
		}
	}
}

// declaredPadding reads the padding-length bytes the way RFC 6716 §3.2.5 says a decoder
// does: each 255 means 254 more and read on, and the first byte below 255 ends it.
func declaredPadding(t *testing.T, packet []byte) (declared, lengthBytes int) {
	t.Helper()
	for i := 2; i < len(packet); i++ {
		lengthBytes++
		if packet[i] == opusPaddingContinues {
			declared += 254
			continue
		}
		return declared + int(packet[i]), lengthBytes
	}
	t.Fatalf("the padding length never terminated in a %d-byte packet", len(packet))
	return 0, 0
}

func TestASilenceFrameRefusesASizeItCannotCarryAStampIn(t *testing.T) {
	t.Parallel()

	for _, size := range []int{-1, 0, 2, minSilenceBytes - 1, protocol.MaxVoiceOpusBytes + 1} {
		if _, err := silenceFrame(size); err == nil {
			t.Errorf("silenceFrame(%d) was accepted; a frame this command cannot time is one it cannot report", size)
		}
	}
}

func TestTheStampSurvivesTheFrameItRidesIn(t *testing.T) {
	t.Parallel()

	packet, err := silenceFrame(96)
	if err != nil {
		t.Fatalf("silenceFrame: %v", err)
	}
	// An unstamped frame reads as unstamped rather than as an instant in 1970, which is
	// what keeps a frame from another sender out of the latency figures.
	if _, ok := readSilenceStamp(packet); ok {
		t.Error("a frame nobody stamped read as stamped")
	}

	sent := time.Now()
	stampSilenceFrame(packet, sent)
	got, ok := readSilenceStamp(packet)
	if !ok {
		t.Fatal("a stamped frame read as unstamped")
	}
	if !got.Equal(sent) {
		t.Errorf("the stamp came back as %v, not the %v that was written", got, sent)
	}
	if len(packet) != 96 {
		t.Errorf("stamping changed the packet's length to %d", len(packet))
	}
}

func TestAFrameTooShortToHoldAStampIsNotMisread(t *testing.T) {
	t.Parallel()

	if _, ok := readSilenceStamp([]byte{0x48, 0x41}); ok {
		t.Error("a two-byte frame was read as carrying a stamp")
	}
	if _, ok := readSilenceStamp(nil); ok {
		t.Error("an absent payload was read as carrying a stamp")
	}
}
