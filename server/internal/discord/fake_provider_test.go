package discord

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"sync"
	"testing"
)

// The fixture values a leak would have to carry. Distinctive on purpose: a grep for any
// of them in a log or a response has exactly one possible source.
const (
	fixtureAccessToken  = "vxh-fixture-access-token-3f7a"
	fixtureRefreshToken = "vxh-fixture-refresh-token-8c2b"
	fixtureEmail        = "player@example.invalid"
	fixtureSubject      = "100000000000000001"
	fixtureUsername     = "eivor"
	fixtureGlobalName   = "Eivor Wolf-Kissed"
)

// fakeDiscord stands in for the provider, and it is the only Discord any test here
// speaks to: a test that reached the real one would be a test of somebody else's
// uptime.
//
// It behaves like the real endpoints on the two things this service depends on — an
// authorization code is redeemable once, and only by whoever presents the verifier
// matching the challenge the authorize URL carried — so the PKCE check under test is
// checked by something that would notice if it were wrong.
type fakeDiscord struct {
	server *httptest.Server

	mu sync.Mutex
	// issued maps an authorization code to the code_challenge it was issued against.
	issued map[string]string
	spent  map[string]bool
	// lastTokenForm is what the token endpoint was last posted, for the assertion that
	// no client secret is ever in it.
	lastTokenForm url.Values
	// lastAuthorization is the header the identity endpoint last saw.
	lastAuthorization string

	// Either handler may be replaced to drive a failure. Nil means the honest one.
	tokenHandler    http.HandlerFunc
	identityHandler http.HandlerFunc

	// The identity the honest handlers resolve to.
	globalName string
	username   string
	subject    string
}

func newFakeDiscord(t *testing.T) *fakeDiscord {
	t.Helper()

	fake := &fakeDiscord{
		issued:     map[string]string{},
		spent:      map[string]bool{},
		globalName: fixtureGlobalName,
		username:   fixtureUsername,
		subject:    fixtureSubject,
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/oauth2/token", func(w http.ResponseWriter, r *http.Request) {
		if h := fake.handlerFor(true); h != nil {
			h(w, r)
			return
		}
		fake.token(w, r)
	})
	mux.HandleFunc("/users/@me", func(w http.ResponseWriter, r *http.Request) {
		if h := fake.handlerFor(false); h != nil {
			h(w, r)
			return
		}
		fake.identity(w, r)
	})

	fake.server = httptest.NewServer(mux)
	t.Cleanup(fake.server.Close)
	return fake
}

func (f *fakeDiscord) handlerFor(token bool) http.HandlerFunc {
	f.mu.Lock()
	defer f.mu.Unlock()
	if token {
		return f.tokenHandler
	}
	return f.identityHandler
}

// config is a flow configuration pointed at this fake.
func (f *fakeDiscord) config() Config {
	return Config{
		ClientID:     "111111111111111111",
		RedirectURI:  "http://127.0.0.1:7780/discord/callback",
		AuthorizeURL: "https://discord.invalid/oauth2/authorize",
		TokenURL:     f.server.URL + "/oauth2/token",
		IdentityURL:  f.server.URL + "/users/@me",
	}
}

// issue is the browser half of the flow, which no test performs for real: it reads the
// state and the challenge out of the authorize URL a Begin produced and hands back an
// authorization code bound to that challenge, exactly as the provider would after
// somebody clicked Authorize.
func (f *fakeDiscord) issue(t *testing.T, start Start) (state, code Secret) {
	t.Helper()

	parsed, err := url.Parse(start.AuthorizeURL)
	if err != nil {
		t.Fatalf("the authorize URL is not a URL: %v", err)
	}
	query := parsed.Query()

	challenge := query.Get("code_challenge")
	if challenge == "" {
		t.Fatal("the authorize URL carries no code_challenge")
	}
	if got := query.Get("state"); got != start.State.Reveal() {
		t.Fatalf("the authorize URL carries a different state than Begin returned")
	}

	f.mu.Lock()
	defer f.mu.Unlock()
	issuedCode := fmt.Sprintf("vxh-fixture-code-%d", len(f.issued)+1)
	f.issued[issuedCode] = challenge
	return start.State, Secret(issuedCode)
}

// token is the honest token endpoint: it redeems a code once, and only for the verifier
// that hashes to the challenge the code was issued against.
func (f *fakeDiscord) token(w http.ResponseWriter, r *http.Request) {
	if err := r.ParseForm(); err != nil {
		http.Error(w, "bad form", http.StatusBadRequest)
		return
	}

	f.mu.Lock()
	f.lastTokenForm = r.PostForm
	code := r.PostForm.Get("code")
	verifier := r.PostForm.Get("code_verifier")
	challenge, known := f.issued[code]
	spent := f.spent[code]
	if known && !spent && rfc7636Challenge(verifier) == challenge {
		f.spent[code] = true
	} else {
		known = false
	}
	f.mu.Unlock()

	if !known {
		// What Discord answers for a code it will not redeem, verifier mismatch
		// included: RFC 6749 section 5.2, a 400 carrying invalid_grant.
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusBadRequest)
		_, _ = w.Write([]byte(`{"error":"invalid_grant"}`))
		return
	}

	writeJSONTo(w, http.StatusOK, map[string]any{
		"access_token":  fixtureAccessToken,
		"token_type":    "Bearer",
		"expires_in":    604800,
		"refresh_token": fixtureRefreshToken,
		"scope":         "identify",
	})
}

// identity is the honest identity endpoint. It answers with an email field the real one
// would only send under a scope this service does not ask for — which is what lets the
// log test assert that an email nobody decoded cannot leak.
func (f *fakeDiscord) identity(w http.ResponseWriter, r *http.Request) {
	f.mu.Lock()
	f.lastAuthorization = r.Header.Get("Authorization")
	subject, username, globalName := f.subject, f.username, f.globalName
	f.mu.Unlock()

	if r.Header.Get("Authorization") != "Bearer "+fixtureAccessToken {
		writeJSONTo(w, http.StatusUnauthorized, map[string]any{"message": "401: Unauthorized"})
		return
	}
	writeJSONTo(w, http.StatusOK, map[string]any{
		"id":            subject,
		"username":      username,
		"global_name":   globalName,
		"discriminator": "0",
		"email":         fixtureEmail,
		"verified":      true,
	})
}

// rfc7636Challenge is the S256 transformation spelled out at the call site rather than
// borrowed from the code under test.
//
// The fake is the thing that decides whether the verifier this service sent is the one
// the challenge was made from, so calling challengeFor here would have it agree with
// whatever that function did — including agreeing with a wrong one.
func rfc7636Challenge(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

func writeJSONTo(w http.ResponseWriter, status int, body any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(body)
}
