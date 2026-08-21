package main

import (
	"net/http"

	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// ticketKeyResponse is what a game server reads once at startup and then keeps.
//
// **This endpoint is what makes verification offline**, which is the whole design: a
// game server holding this key admits a player by checking a signature, so it never
// calls this service on a join and this service being down costs nobody a game. Read
// once and kept, deliberately — a server that fetched it per join would have rebuilt the
// hard dependency the ticket exists to remove.
//
// The algorithm is published rather than assumed, so the reader is told what to do with
// the bytes instead of inferring it from their length.
type ticketKeyResponse struct {
	Algorithm string `json:"algorithm"`

	// PublicKey is the verifying half in lowercase hex, the same 64 characters the
	// startup log prints — so an operator comparing what a game server has stored
	// against what this service says is comparing one string against one string.
	PublicKey string `json:"public_key"`

	// TicketLifetimeSeconds is how long a ticket this service mints is good for.
	//
	// Published for an operator rather than for a verifier: the expiry a game server
	// enforces is inside the ticket and signed, so nothing about this number is trusted
	// by anybody. It is here because "how long does a stolen ticket work" is the
	// question this design's stated cost raises, and it should be answerable without
	// reading the source.
	TicketLifetimeSeconds int `json:"ticket_lifetime_seconds"`
}

// ticketKey publishes the public half of this service's signing key.
//
// GET, and cacheable in the ordinary sense: it reads nothing, changes nothing, and
// answers the same bytes for the life of the pair. It is deliberately unauthenticated —
// a public key is public, and a game server that had to authenticate to read it would
// need a credential from this service before it could stop depending on this service.
//
// **So this is the one response in the service that says how long a copy stays good
// rather than forbidding one**, and it says it explicitly instead of leaving a cache to
// invent a heuristic from the fact that the bytes never change. "The same bytes for the
// life of the pair" is exactly the sentence that stops being true at a rotation, and a
// stale key is a game server refusing every player it should admit. The window, the
// `must-revalidate` and the argument for both are on [cacheTicketKeyFreshness].
//
// s.keys is never nil here; run refuses to build a service without a pair. See the
// [service] doc.
func (s *service) ticketKey(w http.ResponseWriter, _ *http.Request) {
	s.writeJSON(w, http.StatusOK, cacheTicketKeyFreshness, ticketKeyResponse{
		Algorithm:             ticket.Algorithm,
		PublicKey:             s.keys.PublicHex(),
		TicketLifetimeSeconds: int(ticket.Lifetime.Seconds()),
	})
}
