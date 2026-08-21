package main

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

	"github.com/FabioSM46/voxelheim-v2/server/internal/discord"
)

// The fixture values a leak would have to carry. Distinctive on purpose, and every one
// of them under a reserved example domain or clearly synthetic: a grep for any of them
// in a log has exactly one possible source.
const (
	fixtureAccessToken  = "vxh-fixture-access-token-3f7a"
	fixtureRefreshToken = "vxh-fixture-refresh-token-8c2b"
	fixtureEmail        = "player@example.invalid"
	fixtureSubject      = "100000000000000001"
	fixtureUsername     = "eivor"
	fixtureGlobalName   = "Eivor Wolf-Kissed"
)

// fakeDiscord is the provider these tests speak to. internal/discord's own tests carry
// the exhaustive version — every status, every unreadable answer, every timeout; this
// one is the honest provider, because what is under test here is the service around the
// flow rather than the flow itself.
type fakeDiscord struct {
	server *httptest.Server

	mu         sync.Mutex
	issued     map[string]string // authorization code -> the code_challenge it was issued for
	spent      map[string]bool
	globalName string
}

func newFakeDiscord(t *testing.T) *fakeDiscord {
	t.Helper()

	fake := &fakeDiscord{issued: map[string]string{}, spent: map[string]bool{}, globalName: fixtureGlobalName}

	mux := http.NewServeMux()
	mux.HandleFunc("POST /oauth2/token", fake.token)
	mux.HandleFunc("GET /users/@me", fake.identity)

	fake.server = httptest.NewServer(mux)
	t.Cleanup(fake.server.Close)
	return fake
}

func (f *fakeDiscord) config() discord.Config {
	return discord.Config{
		ClientID:     "111111111111111111",
		RedirectURI:  "http://127.0.0.1:7780/discord/callback",
		AuthorizeURL: "https://discord.invalid/oauth2/authorize",
		TokenURL:     f.server.URL + "/oauth2/token",
		IdentityURL:  f.server.URL + "/users/@me",
	}
}

// issue is the browser half of the flow, which no test performs for real: it reads the
// challenge out of the authorize URL and hands back a code bound to it, exactly as the
// provider would once somebody had clicked Authorize.
func (f *fakeDiscord) issue(t *testing.T, authorizeURL string) string {
	t.Helper()

	parsed, err := url.Parse(authorizeURL)
	if err != nil {
		t.Fatalf("the authorize URL is not a URL: %v", err)
	}
	challenge := parsed.Query().Get("code_challenge")
	if challenge == "" {
		t.Fatal("the authorize URL carries no code_challenge")
	}

	f.mu.Lock()
	defer f.mu.Unlock()
	code := fmt.Sprintf("vxh-fixture-code-%d", len(f.issued)+1)
	f.issued[code] = challenge
	return code
}

func (f *fakeDiscord) token(w http.ResponseWriter, r *http.Request) {
	if err := r.ParseForm(); err != nil {
		http.Error(w, "bad form", http.StatusBadRequest)
		return
	}

	f.mu.Lock()
	code := r.PostForm.Get("code")
	challenge, known := f.issued[code]
	if known && !f.spent[code] && s256(r.PostForm.Get("code_verifier")) == challenge {
		f.spent[code] = true
	} else {
		known = false
	}
	f.mu.Unlock()

	if !known {
		writeFakeJSON(w, http.StatusBadRequest, map[string]any{"error": "invalid_grant"})
		return
	}
	writeFakeJSON(w, http.StatusOK, map[string]any{
		"access_token":  fixtureAccessToken,
		"token_type":    "Bearer",
		"expires_in":    604800,
		"refresh_token": fixtureRefreshToken,
		"scope":         "identify",
	})
}

// identity answers with an email the real endpoint would only send under a scope this
// service does not ask for, which is what lets the log test assert that an email nobody
// decoded cannot leak.
func (f *fakeDiscord) identity(w http.ResponseWriter, r *http.Request) {
	if r.Header.Get("Authorization") != "Bearer "+fixtureAccessToken {
		writeFakeJSON(w, http.StatusUnauthorized, map[string]any{"message": "401: Unauthorized"})
		return
	}

	f.mu.Lock()
	globalName := f.globalName
	f.mu.Unlock()

	writeFakeJSON(w, http.StatusOK, map[string]any{
		"id":          fixtureSubject,
		"username":    fixtureUsername,
		"global_name": globalName,
		"email":       fixtureEmail,
	})
}

// s256 is RFC 7636's transformation, spelled out here rather than borrowed from the
// code under test: the fake is what decides whether the verifier this service sent
// matches, so calling the production helper would have it agree with a wrong one.
func s256(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

func writeFakeJSON(w http.ResponseWriter, status int, body any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(body)
}

// spentCode reports whether a code has been redeemed at this provider.
//
// It is how a test asserts that a refusal never reached the network at all — which is
// the property the world check depends on, since an authorization code may be redeemed
// once and a refusal after the redemption would have spent somebody's sign-in.
func (f *fakeDiscord) spentCode(code string) bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.spent[code]
}
