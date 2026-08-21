package discord

import (
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"fmt"
	"io"
	"net/url"
	"time"
)

// secretBytes is how much entropy a state and a PKCE verifier each carry.
//
// Thirty-two bytes, the size internal/identity mints a player token at, encoded
// unpadded base64url: 43 characters, every one of them from RFC 7636's unreserved
// set, which is what makes the same value a legal `code_verifier` and a legal query
// parameter without any further escaping.
const secretBytes = 32

// Begin starts a sign-in: it mints the state and the PKCE verifier, remembers the
// verifier against the state, and returns the URL a browser should be sent to.
//
// The verifier never leaves this service. What goes to the authorize endpoint is its
// SHA-256, base64url-encoded, which is the whole of what PKCE is: the code that comes
// back can only be redeemed by whoever also holds the verifier, and that is this
// process rather than anybody who intercepted the redirect.
func (f *Flow) Begin() (Start, error) {
	state, err := f.mintSecret()
	if err != nil {
		return Start{}, fmt.Errorf("discord: minting the state: %w", err)
	}
	verifier, err := f.mintSecret()
	if err != nil {
		return Start{}, fmt.Errorf("discord: minting the PKCE verifier: %w", err)
	}
	// The third secret, and the only one of the three that neither goes to the provider
	// nor comes back through the browser. See [Start.FinishSecret].
	finish, err := f.mintSecret()
	if err != nil {
		return Start{}, fmt.Errorf("discord: minting the finish secret: %w", err)
	}

	expiresAt := f.now().Add(f.cfg.TTL).UTC()
	if err := f.remember(state, verifier, finish, expiresAt); err != nil {
		return Start{}, err
	}

	// A copy of the endpoint rather than the endpoint: the flow keeps one parsed URL
	// for the life of the process, and mutating it here would make every sign-in after
	// the first carry the one before it.
	authorize := *f.authorize
	query := authorize.Query()
	query.Set("response_type", "code")
	query.Set("client_id", f.cfg.ClientID)
	query.Set("redirect_uri", f.cfg.RedirectURI)
	query.Set("scope", scope)
	query.Set("state", state.Reveal())
	query.Set("code_challenge", challengeFor(verifier))
	query.Set("code_challenge_method", "S256")
	authorize.RawQuery = query.Encode()

	return Start{
		State:        state,
		FinishSecret: finish,
		AuthorizeURL: authorize.String(),
		ExpiresAt:    expiresAt,
	}, nil
}

// Redeem finishes a sign-in and answers with who the provider says this is.
//
// **The pending sign-in is consumed before the provider is called, and that ordering
// is the "a code may be redeemed once" rule.** Taking it afterwards would leave a
// window in which two requests carrying the same state both found the verifier and
// both redeemed — and a provider call that failed halfway would leave the state usable
// for a replay. The cost is the honest one: a sign-in whose token exchange fails for a
// transient reason has to be started again, which is a refusal that says so rather
// than a sign-in that half-succeeded.
//
// A code this service never issued a state for is [ErrNoSuchSignIn] and reaches no
// network at all, so an unknown state cannot be used to make this service issue
// requests.
//
// **finish is what makes the state something other than a bearer credential**, and it
// is required for the reason [Start.FinishSecret] records: `code` and `state` arrive
// together in the provider's redirect, so whoever sees that URL holds both. A redeemer
// must also hold the secret this service answered `start` with, which never entered a
// browser.
func (f *Flow) Redeem(ctx context.Context, state, code, finish Secret) (Identity, error) {
	if state.IsEmpty() || code.IsEmpty() || finish.IsEmpty() {
		// Not ErrNoSuchSignIn: an absent field is a malformed request rather than a
		// sign-in that cannot be found, and the caller answers the two differently.
		// Nothing is looked up here, so nothing is known about whether that state
		// exists — saying "no such sign-in" would be a claim this branch cannot make.
		return Identity{}, fmt.Errorf("%w: the state, the code and the finish secret must all be present", ErrMalformedRequest)
	}

	verifier, err := f.consume(state, finish)
	if err != nil {
		return Identity{}, err
	}

	token, err := f.exchange(ctx, code, verifier)
	if err != nil {
		return Identity{}, err
	}
	return f.identify(ctx, token)
}

// Pending reports how many sign-ins are in flight. It exists for a test and for an
// operator's curiosity, and it reveals nothing about any of them.
func (f *Flow) Pending() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.pending)
}

// mintSecret reads [secretBytes] from the configured source and encodes them.
//
// A failed read is returned, never swallowed. The alternative is the one value this
// package must never produce — an empty state, which every sign-in that failed to mint
// would share, and which would make them all the same pending sign-in.
func (f *Flow) mintSecret() (Secret, error) {
	buf := make([]byte, secretBytes)
	if _, err := io.ReadFull(f.random, buf); err != nil {
		return "", err
	}
	return Secret(base64.RawURLEncoding.EncodeToString(buf)), nil
}

// challengeFor is the S256 code challenge for a verifier: the SHA-256 of its ASCII
// bytes, base64url-encoded without padding, exactly as RFC 7636 section 4.2 states it.
func challengeFor(verifier Secret) string {
	sum := sha256.Sum256([]byte(verifier.Reveal()))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

// remember files a verifier against its state, sweeping what has expired and refusing
// to grow past the cap.
//
// The sweep runs here rather than on a timer because this is the only place the table
// grows: a service nobody is signing in to needs no goroutine ticking over an empty
// map, and one that is busy sweeps on every request. The cap is checked after the
// sweep, so a table full of expired entries is not a refusal.
func (f *Flow) remember(state, verifier, finish Secret, expiresAt time.Time) error {
	f.mu.Lock()
	defer f.mu.Unlock()

	now := f.now()
	for key, held := range f.pending {
		if !held.expiresAt.After(now) {
			delete(f.pending, key)
		}
	}
	if len(f.pending) >= f.cfg.MaxPending {
		return fmt.Errorf("%w: %d are already waiting", ErrTooManyPending, len(f.pending))
	}

	f.pending[state] = pendingSignIn{verifier: verifier, finish: finish, expiresAt: expiresAt}
	return nil
}

// consume takes the verifier a state names, removing it, and refuses a state that is
// unknown, expired, or presented without the secret that started it.
//
// **The whole of it happens under one lock**, which is what keeps "a code may be
// redeemed once" true: a lookup, a comparison and a removal split across three
// acquisitions would let two requests carrying the same state both find the verifier.
//
// **Removed whether or not it had expired**, so that a state cannot be used twice even
// by racing its own expiry — but *not* removed when the finish secret is wrong, and
// that asymmetry is deliberate. Consuming on a mismatch would hand anyone who saw the
// redirect URL a way to destroy a sign-in they cannot complete, which is the same
// attacker the secret exists to stop, achieving a denial instead of a takeover. It is
// safe to leave it: guessing 32 bytes is not something a retry budget helps with.
//
// The state itself is still never compared byte by byte — it is a map key, the shape
// internal/identity resolves a token in. The finish secret is compared, so it is
// compared in constant time: it is the one value here an attacker holds a guess at.
func (f *Flow) consume(state, finish Secret) (Secret, error) {
	f.mu.Lock()
	defer f.mu.Unlock()

	held, found := f.pending[state]
	if !found {
		return "", ErrNoSuchSignIn
	}

	if subtle.ConstantTimeCompare([]byte(held.finish.Reveal()), []byte(finish.Reveal())) != 1 {
		// The same answer as an unknown state, deliberately, and for the reason
		// [ErrNoSuchSignIn] gives: an error that said "that state is real, your secret
		// is not" tells whoever is guessing which guesses are getting warmer.
		return "", ErrNoSuchSignIn
	}
	delete(f.pending, state)

	if !held.expiresAt.After(f.now()) {
		// The same answer as an unknown state, deliberately. See [ErrNoSuchSignIn].
		return "", ErrNoSuchSignIn
	}
	return held.verifier, nil
}

// form is the url.Values a token request is built from. It exists so that the one
// assertion worth making about it — that no client_secret is ever in it — has
// something to be made against.
func (f *Flow) tokenForm(code, verifier Secret) url.Values {
	return url.Values{
		"client_id":     {f.cfg.ClientID},
		"grant_type":    {"authorization_code"},
		"code":          {code.Reveal()},
		"redirect_uri":  {f.cfg.RedirectURI},
		"code_verifier": {verifier.Reveal()},
		// There is no client_secret line here, and there is nowhere for one to come
		// from: Config has no field for it. See the package comment.
	}
}
