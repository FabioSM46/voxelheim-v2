package protocol

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
)

func TestInstanceEntryOfferCarriesItsTermsWhole(t *testing.T) {
	t.Parallel()

	want := InstanceEntryOffer{
		OfferID: 0x0BADC0DE,
		Terms:   SessionBinding{Arch: [3]int32{-4096, 61, 8192}, BossesDefeated: 1, BossesTotal: 3, ResetsAtUnix: 1893456000},
	}
	frame, err := EncodeInstanceEntryOffer(want)
	if err != nil {
		t.Fatalf("EncodeInstanceEntryOffer: %v", err)
	}
	msg, err := Decode(frame)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadInstanceEntryOffer {
		t.Fatalf("Kind = %s", msg.Kind)
	}
	// The offer is server -> client, so this package decodes it only as the strict Go
	// half of the contract; reading it back through the generated bindings is what says
	// the terms survived the trip.
	env := vnet.GetRootAsEnvelope(frame, 0)
	var payload flatbuffers.Table
	if !env.Payload(&payload) {
		t.Fatal("the envelope carries no payload")
	}
	var offer vnet.InstanceEntryOffer
	offer.Init(payload.Bytes, payload.Pos)
	terms := offer.Terms(nil)
	if terms == nil {
		t.Fatal("an offer with no terms says nothing about what is being accepted")
	}
	arch := terms.Arch(nil)
	if offer.OfferId() != want.OfferID || arch == nil ||
		[3]int32{arch.X(), arch.Y(), arch.Z()} != want.Terms.Arch ||
		terms.BossesDefeated() != want.Terms.BossesDefeated ||
		terms.BossesTotal() != want.Terms.BossesTotal ||
		terms.ResetsAtUnix() != want.Terms.ResetsAtUnix {
		t.Fatalf("the terms did not round trip: %+v", offer)
	}
}

// Every decoder invariant schemas/player.fbs states is checked on the way out too. A
// server that emitted one of these would be asking a client to drop a frame it needs.
func TestInstanceEntryOfferRefusesTermsNoClientMayAccept(t *testing.T) {
	t.Parallel()

	sound := SessionBinding{Arch: [3]int32{0, 61, 0}, BossesDefeated: 1, BossesTotal: 2, ResetsAtUnix: 1893456000}
	for name, offer := range map[string]InstanceEntryOffer{
		"no id":              {OfferID: 0, Terms: sound},
		"no total":           {OfferID: 1, Terms: SessionBinding{Arch: sound.Arch, BossesTotal: 0, ResetsAtUnix: sound.ResetsAtUnix}},
		"more dead than are": {OfferID: 1, Terms: SessionBinding{Arch: sound.Arch, BossesDefeated: 3, BossesTotal: 2, ResetsAtUnix: sound.ResetsAtUnix}},
		"no reset":           {OfferID: 1, Terms: SessionBinding{Arch: sound.Arch, BossesDefeated: 1, BossesTotal: 2}},
		"arch off world":     {OfferID: 1, Terms: SessionBinding{Arch: [3]int32{MaxWorldCoordinate + 1, 61, 0}, BossesDefeated: 1, BossesTotal: 2, ResetsAtUnix: sound.ResetsAtUnix}},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if _, err := EncodeInstanceEntryOffer(offer); err == nil {
				t.Fatal("terms no client may accept were encoded anyway")
			}
		})
	}
}

// The answer carries an id and a yes or a no, and an absent `accept` is the no.
func TestInstanceEntryAnswerDecodesIntentAndNothingElse(t *testing.T) {
	t.Parallel()

	for name, want := range map[string]InstanceEntryAnswer{
		"accepted": {OfferID: 42, Accept: true},
		"refused":  {OfferID: 42},
		// A forged id decodes exactly like a real one. Whether the server is holding an
		// offer under it is a decision this package does not make and must not pretend to.
		"forged": {OfferID: 0xFFFF_FFFF_FFFF_FFFF, Accept: true},
		"absent": {},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			msg, err := Decode(EncodeInstanceEntryAnswer(want))
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}
			if msg.Kind != vnet.PayloadInstanceEntryAnswer || msg.EntryAnswer == nil {
				t.Fatalf("Kind = %s, payload %v", msg.Kind, msg.EntryAnswer)
			}
			if *msg.EntryAnswer != want {
				t.Fatalf("answer = %+v, want %+v", *msg.EntryAnswer, want)
			}
		})
	}
}
