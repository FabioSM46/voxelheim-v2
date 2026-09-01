package session

import (
	"strings"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// The first message's own refusals: is this a hello at all, and does it speak this
// protocol.
//
// **An internal test because the answer moved.** Through V6 the welcome answered the
// hello, so a table like this drove the exported Handshake and read a refusal out of it.
// A welcome answers the character choice now — [Welcome] takes no message at all — so
// the rule lives in `unspeakable`, which is unexported because Serve is its only caller
// and because a second caller is exactly what the split exists to prevent.
//
// It is asked before a ticket is verified, and that ordering is the reason it is worth a
// test of its own: a client speaking an older protocol presents no ticket, because a
// ticket is what V7 added, so a version check that ran after the ticket would refuse
// every such client for the one thing it cannot fix.
func TestUnspeakableRefusesWhatCannotBeAHello(t *testing.T) {
	t.Parallel()

	refusals := map[string]struct {
		frame  []byte
		reason vnet.RejectReason
	}{
		// A hello with no version field decodes as Unknown, so the version check is
		// what catches it — the absent case and the wrong case share one path.
		"absent version": {
			frame:  protocol.EncodeClientHello(vnet.ProtocolVersionUnknown, ""),
			reason: vnet.RejectReasonPROTOCOL_MISMATCH,
		},
		"future version": {
			frame:  protocol.EncodeClientHello(vnet.ProtocolVersion(99), "Eivor"),
			reason: vnet.RejectReasonPROTOCOL_MISMATCH,
		},
		// The immediately previous contract is refused at the handshake. V27 cannot name
		// the V28 player-trade intent; discovering that mid-session would close a
		// connection both sides had accepted as compatible.
		"the previous protocol": {
			frame:  protocol.EncodeClientHello(vnet.ProtocolVersion(27), "Eivor"),
			reason: vnet.RejectReasonPROTOCOL_MISMATCH,
		},
		"first message is not a hello": {
			frame:  protocol.EncodeServerWelcome(protocol.Welcome{TickRate: 20, ChunkSize: 32}),
			reason: vnet.RejectReasonBAD_REQUEST,
		},
		// The character phase's own messages, arriving before the phase exists. They are
		// refused here rather than mistaken for a choice, which is the "out of phase is a
		// protocol error" rule schemas/handshake.fbs states for all three of them.
		"a selection before a hello": {
			frame:  protocol.EncodeSelectCharacterRequest(protocol.SelectCharacterRequest{CharacterID: 1}),
			reason: vnet.RejectReasonBAD_REQUEST,
		},
	}

	for name, tc := range refusals {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			msg, err := protocol.Decode(tc.frame)
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}

			reply, refused := unspeakable(msg)
			if !refused {
				t.Fatal("the message was accepted as a hello")
			}
			if got := rejectReason(t, reply); got != tc.reason {
				t.Errorf("Reason = %s, want %s", got, tc.reason)
			}
		})
	}

	t.Run("a current-version hello is not refused", func(t *testing.T) {
		t.Parallel()

		// The direction that keeps the table above from passing by refusing everything.
		msg, err := protocol.Decode(protocol.EncodeClientHello(vnet.ProtocolVersionCurrent, "Eivor"))
		if err != nil {
			t.Fatalf("Decode: %v", err)
		}
		if reply, refused := unspeakable(msg); refused {
			t.Errorf("a hello speaking this protocol was refused with %s", rejectReason(t, reply))
		}
	})
}

// The character phase accepts two messages and answers everything else with a refusal
// that says which two, because a client whose build is sending the wrong thing is the
// only party that can fix it.
func TestTheCharacterPhaseRefusesAnythingElse(t *testing.T) {
	t.Parallel()

	refused := malformedChoice(vnet.PayloadPlayerInput)
	if refused.Reason != vnet.RejectReasonBAD_REQUEST {
		t.Errorf("Reason = %s, want BAD_REQUEST", refused.Reason)
	}
	for _, named := range []string{
		vnet.PayloadSelectCharacterRequest.String(),
		vnet.PayloadCreateCharacterRequest.String(),
		vnet.PayloadPlayerInput.String(),
	} {
		if !strings.Contains(refused.Detail, named) {
			t.Errorf("the refusal %q does not name %s", refused.Detail, named)
		}
	}
}

// rejectReason reads the code out of a refusal frame this package produced.
func rejectReason(t *testing.T, frame []byte) vnet.RejectReason {
	t.Helper()

	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadServerReject {
		t.Fatalf("reply is %s, want %s", env.PayloadType(), vnet.PayloadServerReject)
	}

	var table flatbuffers.Table
	if !env.Payload(&table) {
		t.Fatal("the rejection payload is absent")
	}
	reject := new(vnet.ServerReject)
	reject.Init(table.Bytes, table.Pos)
	return reject.Reason()
}
