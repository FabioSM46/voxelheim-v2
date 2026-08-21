package discord

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
)

// maxProviderResponse bounds what this service will read from the provider.
//
// Both answers are a handful of short JSON fields; sixty-four kilobytes is orders of
// magnitude of headroom. What it buys is that a provider — or something answering in
// its place — that streams forever is a refusal rather than this process's memory.
const maxProviderResponse = 64 << 10

// tokenResponse is the token endpoint's answer, and it is deliberately two fields.
//
// There is no refresh_token here, no expires_in and no scope. Not "ignored" — *absent*:
// a field that is never decoded is a value this process never holds, and so is a value
// that cannot reach a log, a response or a disk. This service asks Discord who somebody
// is once and has no further use for the answer, so keeping a refresh token would be
// keeping a credential for a purpose that does not exist.
type tokenResponse struct {
	AccessToken Secret `json:"access_token"`
	TokenType   string `json:"token_type"`
}

// identityResponse is the identity endpoint's answer, and it has no email field for
// the same reason.
//
// GlobalName is Discord's chosen display name and may be absent or null, in which case
// the account falls back to the username. Both are untrusted display text; encoding/json
// has already replaced any invalid UTF-8 in them with U+FFFD by the time they are read
// here, so what reaches the store is always decodable.
type identityResponse struct {
	ID         string `json:"id"`
	Username   string `json:"username"`
	GlobalName string `json:"global_name"`
}

// exchange redeems the authorization code for an access token.
//
// **A 4xx here is [ErrRejected] and a 5xx is [ErrProviderUnavailable]**, and the split
// is the difference between "this sign-in is not valid" and "ask again later". A
// mismatched PKCE verifier arrives as the first: the token endpoint answers 400
// invalid_grant when the verifier does not hash to the challenge the authorize call
// carried, which is what makes the verifier check real rather than decorative.
//
// 429 is the exception among the 4xx: being rate-limited says nothing about the code,
// so it is reported as the provider being unavailable rather than as a sign-in that
// failed.
func (f *Flow) exchange(ctx context.Context, code, verifier Secret) (Secret, error) {
	body := strings.NewReader(f.tokenForm(code, verifier).Encode())

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, f.cfg.TokenURL, body)
	if err != nil {
		return "", fmt.Errorf("%w: building the token request: %w", ErrProviderUnavailable, err)
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Accept", "application/json")

	resp, err := f.client.Do(req)
	if err != nil {
		// Unreachable, refused, or slower than the client's timeout — one answer,
		// because they are one answer to the person signing in. **The error is not
		// wrapped into the message**: a transport error names the URL it was reaching,
		// which is configuration rather than anybody's data, but it can also carry a
		// proxy's response text, so only its type-level meaning is kept.
		return "", fmt.Errorf("%w: the token request did not complete", ErrProviderUnavailable)
	}
	defer func() { _ = resp.Body.Close() }()

	switch {
	case resp.StatusCode == http.StatusOK:
	case resp.StatusCode == http.StatusTooManyRequests:
		return "", fmt.Errorf("%w: the token endpoint is rate-limiting this service", ErrProviderUnavailable)
	case resp.StatusCode >= 400 && resp.StatusCode < 500:
		// The status and nothing else. A provider's error body is a third party's text
		// and would end up in a log the first time this refusal was investigated.
		return "", fmt.Errorf("%w: the token endpoint answered %d", ErrRejected, resp.StatusCode)
	default:
		return "", fmt.Errorf("%w: the token endpoint answered %d", ErrProviderUnavailable, resp.StatusCode)
	}

	var token tokenResponse
	if err := decodeJSON(resp.Body, &token); err != nil {
		return "", fmt.Errorf("%w: the token endpoint's answer could not be read", ErrProviderUnavailable)
	}
	if token.AccessToken.IsEmpty() {
		return "", fmt.Errorf("%w: the token endpoint answered 200 with no access token", ErrProviderUnavailable)
	}
	// Checked rather than assumed, because the next request's Authorization header is
	// built from it: a provider that answered with some other scheme would have this
	// service present a bearer token as one, and the identity call would fail with a
	// 401 that pointed nowhere.
	if !strings.EqualFold(token.TokenType, "Bearer") {
		return "", fmt.Errorf("%w: the token endpoint answered with a token type this service cannot present", ErrProviderUnavailable)
	}
	return token.AccessToken, nil
}

// identify asks who the access token belongs to.
//
// Every non-200 is [ErrProviderUnavailable] rather than [ErrRejected], including a
// 401. The token being refused here is a token this service was handed seconds ago by
// the same provider: that is the provider contradicting itself, not the person signing
// in getting anything wrong, and telling them their sign-in was invalid would send
// them round the loop for ever.
func (f *Flow) identify(ctx context.Context, token Secret) (Identity, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, f.cfg.IdentityURL, nil)
	if err != nil {
		return Identity{}, fmt.Errorf("%w: building the identity request: %w", ErrProviderUnavailable, err)
	}
	// The one place the access token is revealed, and it goes into a header on a
	// request to the endpoint it came from. Nothing else in this process ever holds it
	// as a plain string.
	req.Header.Set("Authorization", "Bearer "+token.Reveal())
	req.Header.Set("Accept", "application/json")

	resp, err := f.client.Do(req)
	if err != nil {
		return Identity{}, fmt.Errorf("%w: the identity request did not complete", ErrProviderUnavailable)
	}
	defer func() { _ = resp.Body.Close() }()

	if resp.StatusCode != http.StatusOK {
		return Identity{}, fmt.Errorf("%w: the identity endpoint answered %d", ErrProviderUnavailable, resp.StatusCode)
	}

	var who identityResponse
	if err := decodeJSON(resp.Body, &who); err != nil {
		return Identity{}, fmt.Errorf("%w: the identity endpoint's answer could not be read", ErrProviderUnavailable)
	}
	if who.ID == "" {
		// The subject is the key an account is found by. Without one there is nothing
		// to resolve or mint against, and inventing a fallback — the username, say —
		// would key an account on a value the person can change, which is the exact
		// failure the provider identity exists to prevent.
		return Identity{}, fmt.Errorf("%w: the identity endpoint named no user id", ErrProviderUnavailable)
	}

	name := who.GlobalName
	if name == "" {
		name = who.Username
	}
	return Identity{Subject: who.ID, DisplayName: name}, nil
}

// decodeJSON reads one JSON value from a bounded prefix of r.
//
// The limit is applied to the reader rather than checked afterwards, so an endless
// body is stopped as it arrives instead of after it has been held. DisallowUnknownFields
// is deliberately *not* set: a provider is free to add fields, and refusing an answer
// because it grew would break this service on somebody else's release note.
func decodeJSON(r io.Reader, into any) error {
	return json.NewDecoder(io.LimitReader(r, maxProviderResponse)).Decode(into)
}
