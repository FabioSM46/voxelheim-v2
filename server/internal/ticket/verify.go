package ticket

import (
	"crypto/ed25519"
	"fmt"
	"time"
)

// **This file is the property the whole design rests on, and it is a property of what
// it imports.** Verification needs no network, no disk and no clock of its own: a game
// server holding the public key admits a player by doing arithmetic, so the account
// service being down costs nobody a game. Nothing here may import a package that can
// perform I/O — imports_test.go asserts exactly that, over this file and every other
// one in the package that is not the key store.
//
// `now` is a parameter for the same reason internal/auth takes one: what a test writes
// down is what a test reads back, and a function that called time.Now would have a
// dependency this file is trying not to have.

// Verify checks a ticket against a public key and answers what it says.
//
// The order is the rule rather than an implementation detail. The signature is checked
// **before any field is read**, so nothing downstream ever reasons about bytes nobody
// vouched for; the world and the expiry are checked after, because they are decisions
// about a ticket that is genuine rather than questions about whether it is.
//
// Each refusal is its own sentinel because a server answering a handshake has three
// different things to tell a player: this is not a ticket we can read, this ticket is
// for somewhere else, and this ticket has run out. Only the last of the three is
// something waiting will not fix.
//
// world is the caller's own world, so a ticket minted for one server is useless at
// another — which is what stops the operator of one from collecting its players'
// tickets and presenting them somewhere else as those players.
func Verify(pub ed25519.PublicKey, raw []byte, world WorldID, now time.Time) (Claims, error) {
	// crypto/ed25519 panics on a public key of the wrong length, and a panic is not an
	// answer a server can give a connection. Checked first, because it is a question
	// about this server's own configuration rather than about the ticket.
	if len(pub) != ed25519.PublicKeySize {
		return Claims{}, fmt.Errorf("%w, got %d", ErrPublicKeySize, len(pub))
	}
	if world.IsZero() {
		// A verifier that does not know which world it is would accept a ticket for no
		// world, which no mint produces — so this would refuse everything rather than
		// admit anything. It is still an error and not a quiet "no": the configuration
		// is wrong, and an operator should be told which.
		return Claims{}, fmt.Errorf("%w: the verifier names no world", ErrWrongWorld)
	}
	if len(raw) != Size {
		return Claims{}, fmt.Errorf("%w, got %d", ErrTicketSize, len(raw))
	}

	if !ed25519.Verify(pub, raw[:BodySize], raw[BodySize:]) {
		// The bytes are never quoted back. A signature that did not check out is still
		// somebody's bearer credential — a ticket copied from a real session and edited
		// is the case this branch exists for — and an error message reaches a log.
		return Claims{}, ErrBadSignature
	}

	claims, err := decodeBody(raw[:BodySize])
	if err != nil {
		return Claims{}, err
	}
	if claims.World != world {
		// Both ids are named. Neither is a secret — a world id is a digest of a name an
		// operator publishes — and an operator staring at a refusal needs to see which
		// two worlds disagreed.
		return Claims{}, fmt.Errorf("%w: it names %s and this world is %s", ErrWrongWorld, claims.World, world)
	}
	if !now.Before(claims.ExpiresAt) {
		return Claims{}, fmt.Errorf("%w: it expired at %s", ErrExpired, claims.ExpiresAt.Format(time.RFC3339))
	}
	return claims, nil
}
