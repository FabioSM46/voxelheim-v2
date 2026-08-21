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
// **The two configuration answers are sentinels of their own for a different reason**:
// [ErrPublicKeySize] and [ErrVerifierWorld] are not about the ticket at all, and neither
// is anything to tell a player. They are what an operator reads, and they have to be
// distinguishable from the ticket refusals they otherwise look exactly like — see
// [ErrVerifierWorld] for the mistake that produced the second one.
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
		// A verifier that does not know which world it is would be asking whether a
		// ticket names no world — which is exactly what an account ticket does. So this
		// branch is the difference between a misconfigured game server refusing every
		// player and one admitting every account that has ever signed in. It is an
		// error and not a quiet "no": the configuration is wrong, and an operator should
		// be told which.
		//
		// **[ErrVerifierWorld] and not [ErrWrongWorld], because the two are different
		// things to tell an operator and this one was wearing the other's name** (#126).
		// A server started with no world refused every player with "the ticket names
		// another world" — a sentence about the ticket, arriving once per join, that
		// never once mentions that this server names none. The mistake takes a second to
		// fix and is never found, because its symptom is the sentence that means the
		// check is working. [ErrPublicKeySize] two lines up is the same class of question
		// and has had its own sentinel from the start.
		return Claims{}, fmt.Errorf("%w: it was given no world to compare a ticket against", ErrVerifierWorld)
	}

	claims, err := verifySigned(pub, raw)
	if err != nil {
		return Claims{}, err
	}
	if claims.World != world {
		// Both ids are named. Neither is a secret — a world id is a digest of a name an
		// operator publishes — and an operator staring at a refusal needs to see which
		// two worlds disagreed. **An account ticket lands here too**, naming the zero id,
		// which is the refusal it must get from a game server: a ticket for talking to
		// the account service is not a ticket for joining a game.
		return Claims{}, fmt.Errorf("%w: it names %s and this world is %s", ErrWrongWorld, claims.World, world)
	}
	if err := checkExpiry(claims, now); err != nil {
		return Claims{}, err
	}
	return claims, nil
}

// VerifyAnyWorld checks a ticket's signature and its expiry, and does not ask which
// world it names.
//
// **This is the account service's own check, and a game server must never call it.**
// Admitting a player is [Verify]'s job, it takes that server's own world, and the
// comparison is what stops one operator collecting its players' tickets and replaying
// them somewhere else — and what turns an account ticket away at a game server. This
// function drops that comparison entirely, so what it answers is "the account service
// signed this and it has not run out", and nothing more.
//
// The one caller is the account service's own server-list endpoint. It needs to know
// which account is asking and has no world to compare against, because the list is what
// tells a player which worlds there are. Both kinds of ticket are accepted there
// deliberately: somebody already holding a world-scoped ticket should not have to sign in
// again to read the list.
//
// The claims come back with [Claims.World] exactly as it was signed, so a caller that
// does care can still look. Nothing here treats the zero id specially — to this function
// a ticket for a world and a ticket for none are the same question.
func VerifyAnyWorld(pub ed25519.PublicKey, raw []byte, now time.Time) (Claims, error) {
	// The same first question [Verify] asks, and for the same reason: crypto/ed25519
	// panics on a public key of the wrong length.
	if len(pub) != ed25519.PublicKeySize {
		return Claims{}, fmt.Errorf("%w, got %d", ErrPublicKeySize, len(pub))
	}

	claims, err := verifySigned(pub, raw)
	if err != nil {
		return Claims{}, err
	}
	if err := checkExpiry(claims, now); err != nil {
		return Claims{}, err
	}
	return claims, nil
}

// checkExpiry is the pair of questions both verifiers ask about a ticket that is already
// known to be genuine: that it has not run out, and that it does not claim more life than
// this service has ever been able to mint.
//
// **The second question is the one that was missing** (#126). Four bytes of Unix seconds
// hold any instant up to 2106, [encodeBody] writes whatever it is given inside that range,
// and nothing here compared it against anything — so a body carrying 0xFFFFFFFF, signed
// with the real key, verified with seventy-six years left. [Pair.Mint] cannot produce one
// today, which makes this defence in depth rather than a live hole; it is here because the
// game server is the party that must not have to trust the account service beyond its
// signature, and one comparison is the whole cost of not having to.
//
// It lives beside the expiry rather than inside [verifySigned] because both are decisions
// about time, and verifySigned deliberately answers nothing about time — a caller that
// knows which question it is asking is the one holding `now`.
//
// The two comparisons are deliberately asymmetric about clock skew. Expiry has no
// allowance: a ticket that has run out has run out, and with no revocation behind it the
// direction to round in is the strict one. The lifetime bound has [verifierClockSkew],
// because being strict there refuses tickets for being *fresh*, which is a bound that
// breaks every join on a fleet whose clocks are a few seconds apart.
func checkExpiry(claims Claims, now time.Time) error {
	if !now.Before(claims.ExpiresAt) {
		return fmt.Errorf("%w: it expired at %s", ErrExpired, claims.ExpiresAt.Format(time.RFC3339))
	}
	if claims.ExpiresAt.After(now.Add(Lifetime + verifierClockSkew)) {
		// ErrMalformedBody rather than a fifth sentinel: its own doc already says it
		// reports a signed body this build would not have written, and a ticket with
		// more than a lifetime left is exactly that. Nothing about the ticket's *shape*
		// is wrong, which is why the message says what is.
		return fmt.Errorf("%w: it expires at %s, which is further off than the %s this service mints for",
			ErrMalformedBody, claims.ExpiresAt.Format(time.RFC3339), Lifetime)
	}
	return nil
}

// verifySigned is the half both verifiers share: the length, the signature, and reading
// the body of a ticket somebody has now vouched for.
//
// Factored so that there is exactly one place a signature is checked and exactly one
// order in which it happens — **the signature before any field is read**, so nothing
// downstream ever reasons about bytes nobody vouched for. A second copy of these lines
// is a second chance to get that order wrong, and the wrong order is not a thing a test
// of the return value would notice.
//
// It answers nothing about time and nothing about which world: those are decisions about
// a ticket that is already known to be genuine, and they belong to the caller that knows
// which question it is asking.
func verifySigned(pub ed25519.PublicKey, raw []byte) (Claims, error) {
	if len(raw) != Size {
		return Claims{}, fmt.Errorf("%w, got %d", ErrTicketSize, len(raw))
	}
	if !ed25519.Verify(pub, raw[:BodySize], raw[BodySize:]) {
		// The bytes are never quoted back. A signature that did not check out is still
		// somebody's bearer credential — a ticket copied from a real session and edited
		// is the case this branch exists for — and an error message reaches a log.
		return Claims{}, ErrBadSignature
	}
	return decodeBody(raw[:BodySize])
}
