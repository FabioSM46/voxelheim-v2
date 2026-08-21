package discord

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/url"
	"strings"
	"testing"
	"time"
)

// newFlow builds a flow over the fake, and fails the test rather than returning an
// error nobody would check.
func newFlow(t *testing.T, cfg Config) *Flow {
	t.Helper()

	flow, err := New(cfg)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	return flow
}

// signIn runs a whole sign-in against the fake: begin, issue the code the browser would
// have brought back, redeem.
func signIn(t *testing.T, flow *Flow, fake *fakeDiscord) (Identity, error) {
	t.Helper()

	start, err := flow.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	state, code := fake.issue(t, start)
	return flow.Redeem(context.Background(), state, code)
}

// RFC 7636 appendix B, which is the only independent check available on the
// transformation this whole flow rests on: a challenge computed from anything but that
// verifier by that formula is one Discord will not match.
func TestTheChallengeIsTheRFC7636Transformation(t *testing.T) {
	t.Parallel()

	const (
		verifier  = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
		challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
	)
	if got := challengeFor(Secret(verifier)); got != challenge {
		t.Errorf("challengeFor gave %q, want the appendix B value %q", got, challenge)
	}
}

// **The authorize URL is a public client's, and the assertion that matters most is the
// negative one**: nothing in it is a secret, because there is no secret to put in it.
func TestBeginBuildsAPublicClientAuthorizeURL(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	cfg := fake.config()
	flow := newFlow(t, cfg)

	start, err := flow.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}

	parsed, err := url.Parse(start.AuthorizeURL)
	if err != nil {
		t.Fatalf("the authorize URL is not a URL: %v", err)
	}
	if got := parsed.Scheme + "://" + parsed.Host + parsed.Path; got != cfg.AuthorizeURL {
		t.Errorf("the authorize URL points at %q, want %q", got, cfg.AuthorizeURL)
	}

	query := parsed.Query()
	for key, want := range map[string]string{
		"response_type":         "code",
		"client_id":             cfg.ClientID,
		"redirect_uri":          cfg.RedirectURI,
		"scope":                 scope,
		"code_challenge_method": "S256",
		"state":                 start.State.Reveal(),
	} {
		if got := query.Get(key); got != want {
			t.Errorf("the authorize URL carries %s=%q, want %q", key, got, want)
		}
	}
	if query.Get("code_challenge") == "" {
		t.Error("the authorize URL carries no code_challenge")
	}

	// The verifier itself must never leave this process: what goes out is its hash.
	// Reading the pending table is the only way to make that assertion at all.
	flow.mu.Lock()
	held := flow.pending[start.State]
	flow.mu.Unlock()
	if held.verifier.IsEmpty() {
		t.Fatal("Begin filed no verifier against the state it returned")
	}
	if strings.Contains(start.AuthorizeURL, held.verifier.Reveal()) {
		t.Error("the authorize URL carries the PKCE verifier itself")
	}
	if got, want := query.Get("code_challenge"), challengeFor(held.verifier); got != want {
		t.Errorf("the challenge is %q, which is not the S256 of the verifier that was filed", got)
	}

	// A secret would have to be here, and there is nowhere for one to come from.
	for _, forbidden := range []string{"client_secret", "secret", "code_verifier"} {
		if query.Has(forbidden) {
			t.Errorf("the authorize URL carries %s", forbidden)
		}
	}
	if !start.ExpiresAt.After(time.Now()) {
		t.Error("the sign-in expires in the past")
	}
}

// Two sign-ins in flight at once are two states and two verifiers, which is the
// property that lets one service serve more than one person at a time.
func TestEachSignInGetsItsOwnStateAndVerifier(t *testing.T) {
	t.Parallel()

	flow := newFlow(t, newFakeDiscord(t).config())

	first, err := flow.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	second, err := flow.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	if first.State == second.State {
		t.Error("two sign-ins were given the same state")
	}

	flow.mu.Lock()
	defer flow.mu.Unlock()
	if flow.pending[first.State].verifier == flow.pending[second.State].verifier {
		t.Error("two sign-ins were given the same PKCE verifier")
	}
	if len(flow.pending) != 2 {
		t.Errorf("%d sign-ins are pending, want 2", len(flow.pending))
	}
}

// The happy path, and the two assertions about the token request that the acceptance
// criteria turn on: it carries the verifier, and it carries no client secret.
func TestAWholeSignInResolvesTheProviderIdentity(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	flow := newFlow(t, fake.config())

	who, err := signIn(t, flow, fake)
	if err != nil {
		t.Fatalf("Redeem: %v", err)
	}
	if who.Subject != fixtureSubject {
		t.Errorf("the subject is %q, want %q", who.Subject, fixtureSubject)
	}
	if who.DisplayName != fixtureGlobalName {
		t.Errorf("the display name is %q, want %q", who.DisplayName, fixtureGlobalName)
	}

	fake.mu.Lock()
	form := fake.lastTokenForm
	authorization := fake.lastAuthorization
	fake.mu.Unlock()

	if got := form.Get("grant_type"); got != "authorization_code" {
		t.Errorf("the token request asked for grant_type=%q", got)
	}
	if form.Get("code_verifier") == "" {
		t.Error("the token request carried no code_verifier")
	}
	if form.Has("client_secret") {
		t.Error("the token request carried a client_secret; this is a public client")
	}
	if authorization != "Bearer "+fixtureAccessToken {
		t.Error("the identity request did not present the access token it had just been given")
	}

	// The sign-in is spent, whether it succeeded or not.
	if flow.Pending() != 0 {
		t.Errorf("%d sign-ins are still pending after one completed", flow.Pending())
	}
}

// The account key is the subject, and the display name is not part of it. This is the
// provider half of "a changed Discord display name resolves to the same account"; the
// account half is in the account service's own tests, where a store exists.
func TestAChangedDisplayNameIsStillTheSameSubject(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	flow := newFlow(t, fake.config())

	first, err := signIn(t, flow, fake)
	if err != nil {
		t.Fatalf("the first sign-in: %v", err)
	}

	fake.mu.Lock()
	fake.globalName = "Somebody Else Entirely"
	fake.mu.Unlock()

	second, err := signIn(t, flow, fake)
	if err != nil {
		t.Fatalf("the second sign-in: %v", err)
	}

	if second.Subject != first.Subject {
		t.Errorf("the subject changed with the display name: %q then %q", first.Subject, second.Subject)
	}
	if second.DisplayName == first.DisplayName {
		t.Error("the fake did not actually change the display name, so this test proves nothing")
	}
}

// global_name is Discord's chosen display name and may be absent; the username is what
// there always is.
func TestTheUsernameIsTheFallbackDisplayName(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	fake.mu.Lock()
	fake.globalName = ""
	fake.mu.Unlock()

	who, err := signIn(t, newFlow(t, fake.config()), fake)
	if err != nil {
		t.Fatalf("Redeem: %v", err)
	}
	if who.DisplayName != fixtureUsername {
		t.Errorf("the display name is %q, want the username %q", who.DisplayName, fixtureUsername)
	}
}

// **A state this service did not mint reaches no network at all**, which is what stops
// an unknown state from being a way to make this service issue requests on demand.
func TestAnUnknownStateIsRefusedWithoutCallingTheProvider(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	called := false
	fake.mu.Lock()
	fake.tokenHandler = func(http.ResponseWriter, *http.Request) { called = true }
	fake.mu.Unlock()

	flow := newFlow(t, fake.config())
	_, err := flow.Redeem(context.Background(), Secret("a state nobody minted"), Secret("a code"))
	if !errors.Is(err, ErrNoSuchSignIn) {
		t.Errorf("an unknown state gave %v, want ErrNoSuchSignIn", err)
	}
	if called {
		t.Error("an unknown state reached the provider")
	}
}

// A missing field is a malformed request rather than a sign-in that cannot be found,
// and it is refused before anything is looked up.
func TestAnAbsentStateOrCodeIsRefused(t *testing.T) {
	t.Parallel()

	flow := newFlow(t, newFakeDiscord(t).config())
	start, err := flow.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}

	for name, call := range map[string]func() error{
		"no state": func() error {
			_, err := flow.Redeem(context.Background(), "", Secret("a code"))
			return err
		},
		"no code": func() error {
			_, err := flow.Redeem(context.Background(), start.State, "")
			return err
		},
	} {
		if err := call(); !errors.Is(err, ErrNoSuchSignIn) {
			t.Errorf("%s gave %v, want ErrNoSuchSignIn", name, err)
		}
	}

	// The sign-in that was begun is still there: a malformed request must not spend
	// somebody else's pending sign-in, and must not spend its own either.
	if flow.Pending() != 1 {
		t.Errorf("%d sign-ins are pending after two malformed requests, want 1", flow.Pending())
	}
}

// **A code may be redeemed once**, and this service is what says so: the pending
// sign-in is consumed before the provider is called, so the second attempt is refused
// here rather than being sent to Discord to be refused there.
func TestACodeIsRedeemedOnce(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	flow := newFlow(t, fake.config())

	start, err := flow.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	state, code := fake.issue(t, start)

	if _, err := flow.Redeem(context.Background(), state, code); err != nil {
		t.Fatalf("the first redemption: %v", err)
	}

	calls := 0
	fake.mu.Lock()
	honest := fake.token
	fake.tokenHandler = func(w http.ResponseWriter, r *http.Request) {
		calls++
		honest(w, r)
	}
	fake.mu.Unlock()

	if _, err := flow.Redeem(context.Background(), state, code); !errors.Is(err, ErrNoSuchSignIn) {
		t.Errorf("the second redemption gave %v, want ErrNoSuchSignIn", err)
	}
	if calls != 0 {
		t.Error("a replayed code was sent to the provider rather than refused here")
	}
}

// A sign-in that was begun and never finished stops being finishable, and the state is
// gone whether it expired or not — so it cannot be used by racing its own expiry.
func TestAnExpiredSignInIsRefused(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	cfg := fake.config()
	cfg.TTL = time.Minute
	flow := newFlow(t, cfg)

	base := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	flow.now = func() time.Time { return base }

	start, err := flow.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	state, code := fake.issue(t, start)

	flow.now = func() time.Time { return base.Add(2 * time.Minute) }

	if _, err := flow.Redeem(context.Background(), state, code); !errors.Is(err, ErrNoSuchSignIn) {
		t.Errorf("an expired sign-in gave %v, want ErrNoSuchSignIn", err)
	}
	if flow.Pending() != 0 {
		t.Error("an expired sign-in was left in the table after being refused")
	}
}

// **The PKCE verifier is checked, and a mismatch is refused.** The check belongs to the
// provider — it is the only party holding both halves — so the way to test it is to
// send the wrong verifier and assert the refusal comes back as one. Tampering with the
// filed verifier is the only way to produce that from outside, and it is exactly what
// an attacker who intercepted the redirect but not the verifier would be doing.
func TestAMismatchedVerifierIsRefused(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	flow := newFlow(t, fake.config())

	start, err := flow.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	state, code := fake.issue(t, start)

	flow.mu.Lock()
	held := flow.pending[state]
	held.verifier = Secret("a-verifier-that-is-not-the-one-the-challenge-was-made-from")
	flow.pending[state] = held
	flow.mu.Unlock()

	if _, err := flow.Redeem(context.Background(), state, code); !errors.Is(err, ErrRejected) {
		t.Errorf("a mismatched verifier gave %v, want ErrRejected", err)
	}
}

// A code the provider will not redeem, for any of the reasons it would not: the split
// between "this sign-in is not valid" and "ask again later" is what the caller answers
// its own client with.
func TestTheTokenEndpointsStatusDecidesWhichRefusalThisIs(t *testing.T) {
	t.Parallel()

	for name, testCase := range map[string]struct {
		status int
		want   error
	}{
		"a refused code":       {status: http.StatusBadRequest, want: ErrRejected},
		"an unauthorized call": {status: http.StatusUnauthorized, want: ErrRejected},
		"a forbidden call":     {status: http.StatusForbidden, want: ErrRejected},
		"rate limiting":        {status: http.StatusTooManyRequests, want: ErrProviderUnavailable},
		"a broken provider":    {status: http.StatusInternalServerError, want: ErrProviderUnavailable},
		"a bad gateway":        {status: http.StatusBadGateway, want: ErrProviderUnavailable},
		"a redirect":           {status: http.StatusFound, want: ErrProviderUnavailable},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			fake := newFakeDiscord(t)
			status := testCase.status
			fake.mu.Lock()
			fake.tokenHandler = func(w http.ResponseWriter, _ *http.Request) {
				w.Header().Set("Location", "https://elsewhere.invalid/token")
				w.WriteHeader(status)
			}
			fake.mu.Unlock()

			_, err := signIn(t, newFlow(t, fake.config()), fake)
			if !errors.Is(err, testCase.want) {
				t.Errorf("a %d gave %v, want %v", status, err, testCase.want)
			}
		})
	}
}

// Everything the provider can answer with that this service cannot act on. Each is the
// same thing to the person signing in — nobody knows who they are — and none of them
// may produce an identity.
func TestAnAnswerThisServiceCannotReadIsARefusal(t *testing.T) {
	t.Parallel()

	huge := `{"access_token":"` + strings.Repeat("a", maxProviderResponse+1) + `","token_type":"Bearer"}`

	for name, body := range map[string]string{
		"not JSON at all":                "<html>we are down</html>",
		"JSON with no token":             `{"token_type":"Bearer"}`,
		"an empty token":                 `{"access_token":"","token_type":"Bearer"}`,
		"a token type we cannot present": `{"access_token":"x","token_type":"MAC"}`,
		"a body past the limit":          huge,
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			fake := newFakeDiscord(t)
			answer := body
			fake.mu.Lock()
			fake.tokenHandler = func(w http.ResponseWriter, _ *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				_, _ = io.WriteString(w, answer)
			}
			fake.mu.Unlock()

			who, err := signIn(t, newFlow(t, fake.config()), fake)
			if !errors.Is(err, ErrProviderUnavailable) {
				t.Errorf("gave %v, want ErrProviderUnavailable", err)
			}
			if who != (Identity{}) {
				t.Error("a refusal produced an identity")
			}
		})
	}
}

// The identity endpoint's failures are all the provider's, a 401 included: the token it
// is refusing is one it issued seconds ago, so telling the person their sign-in was
// invalid would send them round the loop for ever.
func TestEveryIdentityFailureIsTheProvidersRatherThanTheRequests(t *testing.T) {
	t.Parallel()

	for name, handler := range map[string]http.HandlerFunc{
		"a refused token": func(w http.ResponseWriter, _ *http.Request) {
			w.WriteHeader(http.StatusUnauthorized)
		},
		"a broken endpoint": func(w http.ResponseWriter, _ *http.Request) {
			w.WriteHeader(http.StatusInternalServerError)
		},
		"an answer that is not JSON": func(w http.ResponseWriter, _ *http.Request) {
			_, _ = io.WriteString(w, "<html>we are down</html>")
		},
		"an answer naming no user": func(w http.ResponseWriter, _ *http.Request) {
			writeJSONTo(w, http.StatusOK, map[string]any{"username": "eivor"})
		},
		"an answer with an empty user id": func(w http.ResponseWriter, _ *http.Request) {
			writeJSONTo(w, http.StatusOK, map[string]any{"id": "", "username": "eivor"})
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			fake := newFakeDiscord(t)
			answer := handler
			fake.mu.Lock()
			fake.identityHandler = answer
			fake.mu.Unlock()

			who, err := signIn(t, newFlow(t, fake.config()), fake)
			if !errors.Is(err, ErrProviderUnavailable) {
				t.Errorf("gave %v, want ErrProviderUnavailable", err)
			}
			if who.Subject != "" {
				t.Error("a refusal produced a subject")
			}
		})
	}
}

// A provider nothing is listening on. The fake's own address, with the fake stopped, is
// the closest thing to an unreachable Discord a hermetic test can have.
func TestAnUnreachableProviderIsARefusal(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	cfg := fake.config()
	flow := newFlow(t, cfg)

	start, err := flow.Begin()
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	state, code := fake.issue(t, start)

	fake.server.Close()

	if _, err := flow.Redeem(context.Background(), state, code); !errors.Is(err, ErrProviderUnavailable) {
		t.Errorf("an unreachable provider gave %v, want ErrProviderUnavailable", err)
	}
}

// A provider that answers slower than the sign-in can wait. The handler waits on the
// request's own context so that the client giving up is what ends it, rather than a
// sleep the test then has to outlast.
func TestASlowProviderIsARefusal(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)

	// Released on cleanup, which runs before the fake's own Close because cleanups run
	// last-registered-first. Without it the handler would still be parked when
	// httptest.Server.Close came to wait for it: an http.Client timeout gives up on the
	// client side and does not, on its own, cancel the request the server is serving.
	release := make(chan struct{})
	t.Cleanup(func() { close(release) })

	fake.mu.Lock()
	fake.tokenHandler = func(_ http.ResponseWriter, r *http.Request) {
		select {
		case <-release:
		case <-r.Context().Done():
		}
	}
	fake.mu.Unlock()

	cfg := fake.config()
	cfg.Timeout = 50 * time.Millisecond

	if _, err := signIn(t, newFlow(t, cfg), fake); !errors.Is(err, ErrProviderUnavailable) {
		t.Errorf("a slow provider gave %v, want ErrProviderUnavailable", err)
	}
}

// The start endpoint is unauthenticated by construction, so the table it grows is
// capped — and the cap is checked after the sweep, so a table full of expired entries
// is not a refusal.
func TestPendingSignInsAreCappedAndSweptWhenTheyExpire(t *testing.T) {
	t.Parallel()

	cfg := newFakeDiscord(t).config()
	cfg.MaxPending = 2
	cfg.TTL = time.Minute
	flow := newFlow(t, cfg)

	base := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	flow.now = func() time.Time { return base }

	for i := range cfg.MaxPending {
		if _, err := flow.Begin(); err != nil {
			t.Fatalf("Begin %d: %v", i, err)
		}
	}
	if _, err := flow.Begin(); !errors.Is(err, ErrTooManyPending) {
		t.Errorf("a full table gave %v, want ErrTooManyPending", err)
	}

	flow.now = func() time.Time { return base.Add(2 * time.Minute) }
	if _, err := flow.Begin(); err != nil {
		t.Errorf("Begin after everything expired: %v", err)
	}
	if flow.Pending() != 1 {
		t.Errorf("%d sign-ins are pending after the sweep, want 1", flow.Pending())
	}
}

// failingReader is a source of randomness that has stopped working: the branch
// crypto/rand does not take on any platform this service runs on, and which must never
// be allowed to produce an empty state that every failed sign-in would share.
type failingReader struct{}

func (failingReader) Read([]byte) (int, error) { return 0, errors.New("the entropy pool is gone") }

func TestAFailedMintIsReturnedRatherThanAnEmptyState(t *testing.T) {
	t.Parallel()

	flow := newFlow(t, newFakeDiscord(t).config())
	flow.random = failingReader{}

	start, err := flow.Begin()
	if err == nil {
		t.Fatal("Begin succeeded with no randomness")
	}
	if !start.State.IsEmpty() {
		t.Error("a failed Begin returned a state anyway")
	}
	if flow.Pending() != 0 {
		t.Error("a failed Begin filed a pending sign-in")
	}
}

// A configuration this service could not act on is refused at New, so a typo in an
// endpoint is a startup failure rather than something discovered by the first person
// who tries to play.
func TestNewRefusesAConfigurationItCouldNotActOn(t *testing.T) {
	t.Parallel()

	valid := func() Config {
		return Config{ClientID: "111", RedirectURI: "http://127.0.0.1:7780/discord/callback"}
	}
	if _, err := New(valid()); err != nil {
		t.Fatalf("a valid configuration was refused: %v", err)
	}

	for name, breakIt := range map[string]func(*Config){
		"no client id":                                      func(c *Config) { c.ClientID = "" },
		"a blank client id":                                 func(c *Config) { c.ClientID = "   " },
		"no redirect URI":                                   func(c *Config) { c.RedirectURI = "" },
		"a token URL that is not one":                       func(c *Config) { c.TokenURL = "://not a url" },
		"a token URL with no scheme":                        func(c *Config) { c.TokenURL = "discord.com/api/oauth2/token" },
		"a token URL with no host":                          func(c *Config) { c.TokenURL = "https:///api/oauth2/token" },
		"an identity URL with no scheme":                    func(c *Config) { c.IdentityURL = "discord.com/api/users/@me" },
		"an authorize URL with no scheme":                   func(c *Config) { c.AuthorizeURL = "discord.com/oauth2/authorize" },
		"an endpoint with a scheme net/http cannot send to": func(c *Config) { c.TokenURL = "ftp://discord.com/token" },
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			cfg := valid()
			breakIt(&cfg)
			if _, err := New(cfg); err == nil {
				t.Error("the configuration was accepted")
			}
		})
	}
}

// The zero values fall back to Discord's own endpoints, which is what makes the three
// URL fields a test seam rather than four more things an operator has to type.
func TestTheDefaultsAreDiscords(t *testing.T) {
	t.Parallel()

	flow := newFlow(t, Config{ClientID: "111", RedirectURI: "http://127.0.0.1:7780/discord/callback"})

	if flow.cfg.AuthorizeURL != DefaultAuthorizeURL ||
		flow.cfg.TokenURL != DefaultTokenURL ||
		flow.cfg.IdentityURL != DefaultIdentityURL {
		t.Error("the endpoints did not fall back to Discord's")
	}
	if flow.cfg.Timeout != DefaultTimeout || flow.cfg.TTL != DefaultTTL || flow.cfg.MaxPending != DefaultMaxPending {
		t.Error("the bounds did not fall back to their defaults")
	}
}

// Two sign-ins racing through one flow. The pending table is shared state on an HTTP
// server, so this is the ordinary case rather than the exotic one; run under -race it
// is what says the mutex covers what it claims to.
func TestConcurrentSignInsDoNotTreadOnEachOther(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	flow := newFlow(t, fake.config())

	const runners = 8
	done := make(chan error, runners)
	for range runners {
		go func() {
			start, err := flow.Begin()
			if err != nil {
				done <- err
				return
			}
			state, code := fake.issue(t, start)
			_, err = flow.Redeem(context.Background(), state, code)
			done <- err
		}()
	}
	for range runners {
		if err := <-done; err != nil {
			t.Errorf("a concurrent sign-in failed: %v", err)
		}
	}
	if flow.Pending() != 0 {
		t.Errorf("%d sign-ins are still pending", flow.Pending())
	}
}
