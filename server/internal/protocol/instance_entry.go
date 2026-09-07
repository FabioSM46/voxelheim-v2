package protocol

import (
	"fmt"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
)

// SessionBinding is one saved run a character owes, as the server states it.
//
// The four numbers are the whole of it: where the dungeon is, how far into it the run
// already is, and when it ends. There is deliberately no session id — schemas/player.fbs
// says why — and deliberately no text: what a player reads is the client's sentence, not
// a string this server writes.
type SessionBinding struct {
	Arch           [3]int32
	BossesDefeated uint8
	BossesTotal    uint8
	ResetsAtUnix   int64
}

// InstanceEntryOffer is the crossing the server is offering, and the terms of it.
//
// The id is minted for one crossing by one character. It is not a session, it is not
// derived from one, and it names nothing a second character could ever answer.
type InstanceEntryOffer struct {
	OfferID uint64
	Terms   SessionBinding
}

// InstanceEntryAnswer is copied intent. Whether the id names an offer this server is
// still holding, and whether the character may still cross, are authoritative decisions
// this package does not make: an unknown id and a forged one decode identically here and
// are answered identically there.
//
// Accept has no absent/present distinction on purpose. FlatBuffers decodes a missing
// bool as false, and false is the refusal — the field that decides whether somebody is
// locked to a dungeon for the day fails closed.
type InstanceEntryAnswer struct {
	OfferID uint64
	Accept  bool
}

// validate reports why these terms could not be sent, if they could not.
//
// Every clause is one of the decoder invariants schemas/player.fbs states, checked on
// the way out rather than trusted: a recipient is told to reject a binding whose
// defeated count exceeds its total, and a server that emitted one would be asking a
// client to drop a frame it needs.
func (b SessionBinding) validate() error {
	for _, coordinate := range b.Arch {
		if coordinate < -MaxWorldCoordinate || coordinate > MaxWorldCoordinate {
			return fmt.Errorf("protocol: session binding arch is outside the world")
		}
	}
	if b.BossesTotal == 0 || b.BossesDefeated > b.BossesTotal {
		return fmt.Errorf("protocol: session binding reports %d of %d bosses", b.BossesDefeated, b.BossesTotal)
	}
	if b.ResetsAtUnix == 0 {
		return fmt.Errorf("protocol: session binding has no reset")
	}
	return nil
}

func addSessionBinding(b *flatbuffers.Builder, binding SessionBinding) flatbuffers.UOffsetT {
	vnet.SessionBindingStart(b)
	arch := vnet.CreateBlockCoord(b, binding.Arch[0], binding.Arch[1], binding.Arch[2])
	vnet.SessionBindingAddArch(b, arch)
	vnet.SessionBindingAddBossesDefeated(b, binding.BossesDefeated)
	vnet.SessionBindingAddBossesTotal(b, binding.BossesTotal)
	vnet.SessionBindingAddResetsAtUnix(b, binding.ResetsAtUnix)
	return vnet.SessionBindingEnd(b)
}

// EncodeInstanceEntryOffer builds one prompt. A zero id is refused rather than sent: it
// is the value a client reads as "no offer", so an offer carrying one could only ever be
// answered with the refusal the whole message exists to avoid.
func EncodeInstanceEntryOffer(offer InstanceEntryOffer) ([]byte, error) {
	if offer.OfferID == 0 {
		return nil, fmt.Errorf("protocol: entry offer has no id")
	}
	if err := offer.Terms.validate(); err != nil {
		return nil, err
	}
	b := flatbuffers.NewBuilder(128)
	terms := addSessionBinding(b, offer.Terms)
	vnet.InstanceEntryOfferStart(b)
	vnet.InstanceEntryOfferAddOfferId(b, offer.OfferID)
	vnet.InstanceEntryOfferAddTerms(b, terms)
	return finishEnvelope(b, vnet.PayloadInstanceEntryOffer, vnet.InstanceEntryOfferEnd(b)), nil
}

// EncodeInstanceEntryAnswer builds one answer. The server never sends this payload; it
// exists so that the wire shape a client has to produce is exercised by this repository's
// own tests rather than only by a client nobody here compiles.
func EncodeInstanceEntryAnswer(answer InstanceEntryAnswer) []byte {
	b := flatbuffers.NewBuilder(64)
	vnet.InstanceEntryAnswerStart(b)
	vnet.InstanceEntryAnswerAddOfferId(b, answer.OfferID)
	vnet.InstanceEntryAnswerAddAccept(b, answer.Accept)
	return finishEnvelope(b, vnet.PayloadInstanceEntryAnswer, vnet.InstanceEntryAnswerEnd(b))
}
