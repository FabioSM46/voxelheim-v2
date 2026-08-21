// Package discord asks Discord who somebody is, once, and keeps nothing.
//
// # What this package is
//
// One half of OAuth 2.0 Authorization Code with PKCE (RFC 7636), as a **public
// client**. [Flow.Begin] mints the `state` and the PKCE verifier and returns the URL a
// browser should be sent to; [Flow.Redeem] takes the `state` and the authorization
// code back, exchanges the code for an access token, asks Discord for the identity
// behind it, and returns that identity. Three plain HTTPS calls — authorize, token,
// identity — so net/http and encoding/json are the whole of what it needs. An OAuth
// library here would be larger than the code it replaced and would need auditing for
// a flow this small.
//
// # There is no client secret, and that is the point
//
// A public client authenticates with nothing but its client id, and PKCE is what
// stands in for the secret: the verifier is minted here, only its SHA-256 is sent to
// the authorize endpoint, and the token endpoint will not redeem a code without the
// verifier that matches. A secret shipped to players is not a secret, so there is
// none to ship — [New] takes no field for one and no call here sends one.
//
// # The redirect lands on the player's machine, not on this service
//
// The client opens the authorize URL in a browser and catches the redirect on a
// loopback listener of its own, which is why this service needs no public callback
// URL and why the flow works behind a home router. This service only ever sees the
// authorization code the client hands back.
//
// # Nothing from the provider is kept, and nothing from it is printed
//
// The access token lives for the length of one identity call and is then dropped;
// the refresh token, the granted scope and the email are **never decoded at all**,
// so there is no field for them to leak out of. Everything that is held —
// authorization code, access token, PKCE verifier, state — is a [Secret], which
// redacts itself through fmt, through log/slog and through encoding/json. A
// provider's response body is a third party's text that would end up in a log, so a
// refusal names the HTTP status and nothing from the body.
//
// # A refusal, never a half-succeeded sign-in
//
// Every way this can fail is one of five sentinels — [ErrMalformedRequest],
// [ErrNoSuchSignIn], [ErrRejected],
// [ErrProviderUnavailable], [ErrTooManyPending] — and none of them returns an
// identity. A provider that is unreachable, slow or answering an error is the third
// one; the caller has nothing to key an account on and mints nothing.
//
// # This package imports nothing of ours
//
// It is a leaf, like internal/identity: it never opens the accounts directory, never
// learns what an account is, and never touches internal/auth. cmd/voxelheim-auth is
// the one place the identity this returns meets the store that records it, and
// imports_test.go is what says so.
package discord

import (
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"
)

// Provider is this service's own name for Discord, and the value that goes into the
// provider half of an account's identity.
//
// Lowercase and constant, because internal/auth refuses a provider name that is not
// drawn from that vocabulary and refuses it rather than normalising it: "Discord" and
// "discord" arriving as two spellings would be two accounts for one person.
const Provider = "discord"

// Discord's own endpoints, which are what [Config] falls back to.
//
// Overridable as struct fields rather than baked in, because a test that reaches the
// real Discord is not a test: every test here points these at an httptest.Server.
const (
	DefaultAuthorizeURL = "https://discord.com/oauth2/authorize"
	DefaultTokenURL     = "https://discord.com/api/oauth2/token"
	DefaultIdentityURL  = "https://discord.com/api/users/@me"

	// DefaultTimeout bounds each of the two calls this service makes. A sign-in
	// happens while a person watches, so the honest answer to a provider that has
	// gone quiet is a refusal rather than a request that hangs until the HTTP
	// server's own write deadline kills it.
	DefaultTimeout = 10 * time.Second

	// DefaultTTL is how long a started sign-in can be finished within. It bounds a
	// browser round trip and a person reading a consent screen, and nothing longer:
	// every unfinished sign-in is server state that somebody who never intended to
	// sign in can ask for.
	DefaultTTL = 10 * time.Minute

	// DefaultMaxPending caps how many sign-ins can be in flight at once. The start
	// endpoint is unauthenticated by construction — a sign-in is how somebody becomes
	// known, so there is nobody to authenticate yet — which makes an uncapped table
	// a way to spend this service's memory for the price of an HTTP request.
	DefaultMaxPending = 4096
)

// scope is what this service asks Discord for, and it is the narrowest scope that
// answers the only question being asked.
//
// `identify` returns the user id and the display name. It does **not** return an
// email, which is why the identity response below has no field for one: the scope and
// the struct agree, so there is no email to log even if a provider sent one anyway.
const scope = "identify"

// The four ways a sign-in can fail, and no fifth. Each maps to exactly one answer the
// caller gives its own client, which is why they are sentinels rather than strings.
var (
	// ErrNoSuchSignIn reports a state this service did not mint, one that has expired,
	// or one that has already been redeemed. The three are deliberately one answer: an
	// error that distinguished them would tell whoever is guessing which guesses are
	// getting warmer.
	ErrNoSuchSignIn = errors.New("discord: no sign-in is waiting for that state")

	// ErrMalformedRequest reports a redemption that is missing a field, which is a
	// different thing from one whose state cannot be found: nothing was looked up, so
	// nothing can be said about whether that sign-in exists. Kept apart because the
	// caller answers the two differently and because [Redeem]'s own comment promised
	// the distinction while the code collapsed it (found in review on #122).
	ErrMalformedRequest = errors.New("discord: the redemption is missing a field")

	// ErrRejected reports that Discord refused the authorization code — it was wrong,
	// it has been used, it has expired, or the PKCE verifier does not match the
	// challenge the authorize call carried.
	ErrRejected = errors.New("discord: the provider refused the authorization code")

	// ErrProviderUnavailable reports that Discord could not be reached, did not answer
	// in time, answered with an error, or answered with something this service cannot
	// read. Every one of those is the same thing to the person signing in: nobody knows
	// who they are yet, and no account is created.
	ErrProviderUnavailable = errors.New("discord: the provider could not be reached")

	// ErrTooManyPending reports that too many sign-ins are already in flight. It is a
	// refusal of this request rather than a fault: the next one after some of them
	// expire will be answered.
	ErrTooManyPending = errors.New("discord: too many sign-ins are already in flight")
)

// Config is everything a [Flow] needs, and deliberately not a client secret.
//
// The three endpoints and the three bounds are fields rather than constants so that a
// test can point the flow at an httptest.Server and at a timeout it can outlast. Each
// zero value falls back to the Default above it; ClientID and RedirectURI have no
// default, because guessing either would be guessing at somebody's Discord
// application.
type Config struct {
	// ClientID is the Discord application's public id. A public client has this and
	// nothing else — see the package comment.
	ClientID string

	// RedirectURI is where Discord sends the browser once the person has agreed. It is
	// a loopback address on the *player's* machine, and it must be one the Discord
	// application has registered.
	RedirectURI string

	AuthorizeURL string
	TokenURL     string
	IdentityURL  string

	// Timeout bounds each call to the provider.
	Timeout time.Duration

	// TTL is how long a started sign-in stays finishable.
	TTL time.Duration

	// MaxPending caps how many sign-ins may be in flight at once.
	MaxPending int
}

// Identity is who Discord says somebody is: the two values this service has any use
// for, and no third.
type Identity struct {
	// Subject is Discord's own id for this person — a snowflake, stable across every
	// name they ever choose. It is the key an account is found by, which is what makes
	// a changed display name resolve to the same account.
	Subject string

	// DisplayName is what to call them, as Discord last reported it. Untrusted display
	// text: nothing keys on it, and nothing here logs it.
	DisplayName string
}

// Start is a sign-in that has been begun and not yet finished.
type Start struct {
	// State names this sign-in. The client carries it through the browser and hands it
	// back with the authorization code, which is what binds the code that comes back
	// to the verifier that was minted for it.
	State Secret

	// FinishSecret is what proves the caller redeeming this sign-in is the one that
	// started it, and **it is deliberately the one value here that never travels
	// through the browser**.
	//
	// Without it the state was a bearer credential, which is the hole the review on
	// #122 found. The provider's redirect carries `code` and `state` in one URL, so
	// anything that observes that URL — a process watching the loopback callback,
	// browser history, a referer — held everything `finish` asked for. The verifier
	// living on this side is what makes the exchange safe from a stolen *code*; it does
	// nothing about a stolen *state*, because the client presented no secret of its
	// own. This is that secret: minted here, answered to the caller of `start`, and
	// required by `finish`.
	FinishSecret Secret

	// AuthorizeURL is where the browser goes.
	AuthorizeURL string

	// ExpiresAt is when this sign-in stops being finishable.
	ExpiresAt time.Time
}

// Flow is one account service's Discord sign-in: the configuration, the HTTP client
// that talks to the provider, and the sign-ins currently in flight.
//
// Safe for concurrent use. Every HTTP handler in the service shares one.
type Flow struct {
	cfg Config

	// authorize is the parsed authorize endpoint, copied per call rather than reparsed.
	// Validated once in [New], so building a URL cannot fail later.
	authorize *url.URL

	client *http.Client

	// now and random are the two things this package cannot be tested against without
	// injecting: an expiry that a test can reach without sleeping, and the failed read
	// from crypto/rand that must never be allowed to become an empty state. Both are
	// real on every path but a test's.
	now    func() time.Time
	random io.Reader

	mu      sync.Mutex
	pending map[Secret]pendingSignIn
}

// pendingSignIn is one sign-in waiting for its authorization code.
type pendingSignIn struct {
	verifier Secret
	// finish is compared against what the redeemer presents. See [Start.FinishSecret].
	finish    Secret
	expiresAt time.Time
}

// New builds a flow, refusing a configuration it could not act on.
//
// Everything that can be wrong with an endpoint is decided here rather than at the
// first sign-in: a service whose token URL is a typo should say so at startup, not
// discover it the first time somebody tries to play.
func New(cfg Config) (*Flow, error) {
	if strings.TrimSpace(cfg.ClientID) == "" {
		return nil, errors.New("discord: the client id must be named; a public client has nothing else to identify itself with")
	}
	if strings.TrimSpace(cfg.RedirectURI) == "" {
		return nil, errors.New("discord: the redirect URI must be named; it is where the provider sends the browser back to")
	}
	// Through `endpoint` rather than a bare url.Parse, which accepts a relative
	// reference: `discord.example/callback` parses cleanly and names no host, so a
	// configuration that can only fail at the *first sign-in* — when the provider
	// refuses an unusable redirect_uri — passed this check. The comment above says
	// misconfiguration is decided here, and now it is (found in review on #122).
	if _, err := endpoint("redirect URI", cfg.RedirectURI); err != nil {
		return nil, err
	}

	cfg.AuthorizeURL = orDefault(cfg.AuthorizeURL, DefaultAuthorizeURL)
	cfg.TokenURL = orDefault(cfg.TokenURL, DefaultTokenURL)
	cfg.IdentityURL = orDefault(cfg.IdentityURL, DefaultIdentityURL)
	if cfg.Timeout <= 0 {
		cfg.Timeout = DefaultTimeout
	}
	if cfg.TTL <= 0 {
		cfg.TTL = DefaultTTL
	}
	if cfg.MaxPending <= 0 {
		cfg.MaxPending = DefaultMaxPending
	}

	authorize, err := endpoint("authorize", cfg.AuthorizeURL)
	if err != nil {
		return nil, err
	}
	if _, err := endpoint("token", cfg.TokenURL); err != nil {
		return nil, err
	}
	if _, err := endpoint("identity", cfg.IdentityURL); err != nil {
		return nil, err
	}

	return &Flow{
		cfg:       cfg,
		authorize: authorize,
		client: &http.Client{
			Timeout: cfg.Timeout,
			// A token exchange does not follow redirects. Go already strips the
			// Authorization header across hosts, so this is not about the bearer token
			// leaking; it is that an endpoint answering 302 is an endpoint that is not
			// the one this service was configured with, and following it silently would
			// send a client id and an authorization code somewhere nobody chose.
			CheckRedirect: func(*http.Request, []*http.Request) error {
				return http.ErrUseLastResponse
			},
		},
		now:     time.Now,
		random:  rand.Reader,
		pending: make(map[Secret]pendingSignIn),
	}, nil
}

// orDefault is value, or fallback when value is blank.
func orDefault(value, fallback string) string {
	if strings.TrimSpace(value) == "" {
		return fallback
	}
	return value
}

// endpoint parses one provider endpoint, refusing anything net/http could not send a
// request to.
//
// The scheme is checked rather than required to be https, because every test in this
// package points these at an httptest.Server on 127.0.0.1 — and a rule that forced
// https would be one the tests had to be exempted from, which is a rule that no longer
// describes what runs.
func endpoint(name, raw string) (*url.URL, error) {
	parsed, err := url.Parse(raw)
	if err != nil {
		return nil, fmt.Errorf("discord: the %s endpoint %q is not a URL: %w", name, raw, err)
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return nil, fmt.Errorf("discord: the %s endpoint %q must be an http or https URL", name, raw)
	}
	if parsed.Host == "" {
		return nil, fmt.Errorf("discord: the %s endpoint %q names no host", name, raw)
	}
	return parsed, nil
}
