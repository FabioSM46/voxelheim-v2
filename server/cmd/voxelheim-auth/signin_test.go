package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"io/fs"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/auth"
	"github.com/FabioSM46/voxelheim-v2/server/internal/discord"
)

// signInService is a service with a real account store in a directory of the test's
// own and a sign-in flow pointed at the fake provider.
//
// The store is real rather than a double: what most of this file is about is what ends
// up on disk after a sign-in — one account, or none — and a double would be asserting
// against the test's own idea of a store.
func signInService(t *testing.T, fake *fakeDiscord, log *slog.Logger) (*service, string) {
	t.Helper()

	authDir := filepath.Join(t.TempDir(), "auth")
	accounts, err := auth.OpenStore(authDir)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	flow, err := discord.New(fake.config())
	if err != nil {
		t.Fatalf("discord.New: %v", err)
	}
	return &service{log: log, signin: &signIn{flow: flow, accounts: accounts}}, accounts.Dir()
}

// call drives one request through the real route table, which is the only way these
// tests reach a handler: a handler called directly is a handler tested without the
// method and the pattern that CI's route-table test pins.
func call(t *testing.T, svc *service, method, path, body string) *httptest.ResponseRecorder {
	t.Helper()

	var req *http.Request
	if body == "" {
		req = httptest.NewRequest(method, path, nil)
	} else {
		req = httptest.NewRequest(method, path, strings.NewReader(body))
	}
	rec := httptest.NewRecorder()
	newMux(svc.routes()).ServeHTTP(rec, req)
	return rec
}

// start runs the start endpoint and returns what a client would have been given.
func start(t *testing.T, svc *service) startResponse {
	t.Helper()

	rec := call(t, svc, http.MethodPost, "/v1/signin/discord/start", "")
	if rec.Code != http.StatusOK {
		t.Fatalf("POST /start answered %d: %s", rec.Code, rec.Body.String())
	}
	var answer startResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &answer); err != nil {
		t.Fatalf("reading the start response: %v", err)
	}
	return answer
}

// finish runs the finish endpoint with a state, a code and the secret `start` answered
// with. The secret is the one field the browser never carried; see startResponse.
func finish(t *testing.T, svc *service, state, code, finishSecret string) *httptest.ResponseRecorder {
	t.Helper()

	body, err := json.Marshal(map[string]string{
		"state":         state,
		"code":          code,
		"finish_secret": finishSecret,
	})
	if err != nil {
		t.Fatalf("building the finish request: %v", err)
	}
	return call(t, svc, http.MethodPost, "/v1/signin/discord/finish", string(body))
}

// signInOnce runs a whole sign-in — start, the browser round trip the fake stands in
// for, finish — and returns the account it settled on.
func signInOnce(t *testing.T, svc *service, fake *fakeDiscord) finishResponse {
	t.Helper()

	begun := start(t, svc)
	rec := finish(t, svc, begun.State, fake.issue(t, begun.AuthorizeURL), begun.FinishSecret)
	if rec.Code != http.StatusOK {
		t.Fatalf("POST /finish answered %d: %s", rec.Code, rec.Body.String())
	}
	var answer finishResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &answer); err != nil {
		t.Fatalf("reading the finish response: %v", err)
	}
	return answer
}

// refusalCode is the machine-readable code a refusal carried, and it fails the test
// when the answer is not one.
func refusalCode(t *testing.T, rec *httptest.ResponseRecorder) string {
	t.Helper()

	if got := rec.Header().Get("Content-Type"); got != "application/json" {
		t.Errorf("a refusal answered with content type %q, want application/json", got)
	}
	var answer errorResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &answer); err != nil {
		t.Fatalf("reading a refusal: %v (body %q)", err, rec.Body.String())
	}
	return answer.Error
}

// accountFiles is every account record on disk, which is how "no account was created"
// is asserted rather than assumed.
func accountFiles(t *testing.T, dir string) []string {
	t.Helper()

	var found []string
	err := filepath.WalkDir(dir, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if !entry.IsDir() {
			found = append(found, path)
		}
		return nil
	})
	if err != nil && !os.IsNotExist(err) {
		t.Fatalf("reading the accounts directory: %v", err)
	}
	return found
}

// The start endpoint's whole job: somewhere to send the browser, and the state that
// binds what comes back to the verifier this service kept.
func TestStartNamesWhereToSendTheBrowser(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	svc, _ := signInService(t, fake, discard())

	begun := start(t, svc)
	if begun.State == "" {
		t.Error("the start response carries no state")
	}
	if !begun.ExpiresAt.After(time.Now()) {
		t.Error("the sign-in expires in the past")
	}

	parsed, err := url.Parse(begun.AuthorizeURL)
	if err != nil {
		t.Fatalf("the authorize URL is not a URL: %v", err)
	}
	query := parsed.Query()
	if query.Get("state") != begun.State {
		t.Error("the authorize URL carries a different state than the response")
	}
	if query.Get("code_challenge_method") != "S256" {
		t.Error("the authorize URL does not ask for S256 PKCE")
	}
	// The property the whole design turns on: this is a public client, and there is
	// nothing secret to put in a URL a browser is about to be handed.
	if query.Has("client_secret") {
		t.Error("the authorize URL carries a client_secret")
	}
}

// **First sign-in mints, second resolves.** The account id is what the rest of the game
// will carry, so the same person coming back has to get the same one.
func TestAFirstSignInMintsAnAccountAndTheNextResolvesIt(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	svc, accountsDir := signInService(t, fake, discard())

	first := signInOnce(t, svc, fake)
	if !first.Created {
		t.Error("the first sign-in did not create an account")
	}
	if first.AccountID == "" {
		t.Fatal("the first sign-in named no account")
	}
	if first.DisplayName != fixtureGlobalName {
		t.Errorf("the account is named %q, want %q", first.DisplayName, fixtureGlobalName)
	}

	second := signInOnce(t, svc, fake)
	if second.Created {
		t.Error("the second sign-in created a second account for the same person")
	}
	if second.AccountID != first.AccountID {
		t.Errorf("the same person got two account ids: %q then %q", first.AccountID, second.AccountID)
	}
	if files := accountFiles(t, accountsDir); len(files) != 1 {
		t.Errorf("%d account records exist after two sign-ins by one person, want 1", len(files))
	}
}

// **The provider identity is the key, and nothing keys on a name.** A person who
// renames themselves on Discord is the same person here.
func TestAChangedDisplayNameResolvesTheSameAccount(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	svc, accountsDir := signInService(t, fake, discard())

	first := signInOnce(t, svc, fake)

	fake.mu.Lock()
	fake.globalName = "A Completely Different Name"
	fake.mu.Unlock()

	second := signInOnce(t, svc, fake)
	if second.AccountID != first.AccountID {
		t.Errorf("a renamed person got a new account: %q then %q", first.AccountID, second.AccountID)
	}
	if second.Created {
		t.Error("a renamed person had a second account minted for them")
	}
	if files := accountFiles(t, accountsDir); len(files) != 1 {
		t.Errorf("%d account records exist, want 1", len(files))
	}
}

// A state this service never minted, and one it minted for somebody else's flow.
func TestAStateThisServiceDidNotMintIsRefused(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	svc, accountsDir := signInService(t, fake, discard())

	begun := start(t, svc)
	code := fake.issue(t, begun.AuthorizeURL)

	rec := finish(t, svc, "a state nobody minted", code, "a finish secret")
	if rec.Code != http.StatusBadRequest {
		t.Errorf("an unknown state answered %d, want 400", rec.Code)
	}
	if got := refusalCode(t, rec); got != errSignInNotFound {
		t.Errorf("an unknown state answered %q, want %q", got, errSignInNotFound)
	}
	if files := accountFiles(t, accountsDir); len(files) != 0 {
		t.Errorf("a refused sign-in left %d account records behind", len(files))
	}
}

// The endpoint half of the state-is-not-a-bearer-credential rule, and the two refusals
// it answers with are deliberately different codes.
//
// A body with no `finish_secret` is `malformed_request`: nothing was looked up, so this
// service cannot say whether that sign-in exists. A body with the *wrong* secret is
// `sign_in_not_found`, the same answer an unknown state gets, because an answer that
// distinguished them would tell whoever is guessing that the state half is right.
func TestFinishingWithoutTheSecretIsRefusedAndTheSignInSurvives(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	svc, accountsDir := signInService(t, fake, discard())

	begun := start(t, svc)
	code := fake.issue(t, begun.AuthorizeURL)

	missing := finish(t, svc, begun.State, code, "")
	if missing.Code != http.StatusBadRequest {
		t.Errorf("a redemption with no finish secret answered %d, want 400", missing.Code)
	}
	if got := refusalCode(t, missing); got != errMalformedRequest {
		t.Errorf("a redemption with no finish secret answered %q, want %q", got, errMalformedRequest)
	}

	wrong := finish(t, svc, begun.State, code, "a secret nobody issued")
	if wrong.Code != http.StatusBadRequest {
		t.Errorf("a redemption with the wrong finish secret answered %d, want 400", wrong.Code)
	}
	if got := refusalCode(t, wrong); got != errSignInNotFound {
		t.Errorf("a redemption with the wrong finish secret answered %q, want %q", got, errSignInNotFound)
	}

	// Neither refusal minted anything, and neither spent the sign-in: the client that
	// started it still finishes.
	if files := accountFiles(t, accountsDir); len(files) != 0 {
		t.Errorf("%d account records exist after two refusals, want none", len(files))
	}
	if rec := finish(t, svc, begun.State, code, begun.FinishSecret); rec.Code != http.StatusOK {
		t.Fatalf("the sign-in that began this test answered %d: %s", rec.Code, rec.Body.String())
	}
}

// **A code may be redeemed once.** The second attempt is refused by this service rather
// than sent to Discord to be refused there, which is what makes the rule this service's
// and not the provider's.
func TestASignInCanOnlyBeFinishedOnce(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	svc, accountsDir := signInService(t, fake, discard())

	begun := start(t, svc)
	code := fake.issue(t, begun.AuthorizeURL)

	if rec := finish(t, svc, begun.State, code, begun.FinishSecret); rec.Code != http.StatusOK {
		t.Fatalf("the first finish answered %d: %s", rec.Code, rec.Body.String())
	}

	rec := finish(t, svc, begun.State, code, begun.FinishSecret)
	if rec.Code != http.StatusBadRequest {
		t.Errorf("a replayed sign-in answered %d, want 400", rec.Code)
	}
	if got := refusalCode(t, rec); got != errSignInNotFound {
		t.Errorf("a replayed sign-in answered %q, want %q", got, errSignInNotFound)
	}
	if files := accountFiles(t, accountsDir); len(files) != 1 {
		t.Errorf("%d account records exist after one sign-in and one replay, want 1", len(files))
	}
}

// A request this service cannot read is a refusal that says so, and leaves nothing
// behind. The oversized case is the one the body cap exists for.
func TestAMalformedSignInRequestIsRefused(t *testing.T) {
	t.Parallel()

	oversized, err := json.Marshal(map[string]string{
		"state": strings.Repeat("a", maxSignInRequestBytes+1),
		"code":  "x",
	})
	if err != nil {
		t.Fatalf("building the oversized request: %v", err)
	}

	for name, body := range map[string]string{
		"an empty body":         "",
		"text that is not JSON": "not json at all",
		"a JSON array":          `["state","code"]`,
		"a truncated object":    `{"state":"abc"`,
		"a body past the cap":   string(oversized),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			fake := newFakeDiscord(t)
			svc, accountsDir := signInService(t, fake, discard())

			rec := call(t, svc, http.MethodPost, "/v1/signin/discord/finish", body)
			if rec.Code != http.StatusBadRequest {
				t.Errorf("answered %d, want 400", rec.Code)
			}
			if got := refusalCode(t, rec); got != errMalformedRequest {
				t.Errorf("answered %q, want %q", got, errMalformedRequest)
			}
			if files := accountFiles(t, accountsDir); len(files) != 0 {
				t.Errorf("a malformed request left %d account records behind", len(files))
			}
		})
	}
}

// **A provider that is not there is a refusal, never a half-succeeded sign-in.** The
// assertion that matters is the second one: nothing was written down about a person
// nobody has identified.
func TestAProviderThatIsNotThereIsARefusalAndNotAnAccount(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	svc, accountsDir := signInService(t, fake, discard())

	begun := start(t, svc)
	code := fake.issue(t, begun.AuthorizeURL)
	fake.server.Close()

	rec := finish(t, svc, begun.State, code, begun.FinishSecret)
	if rec.Code != http.StatusBadGateway {
		t.Errorf("an unreachable provider answered %d, want 502", rec.Code)
	}
	if got := refusalCode(t, rec); got != errProviderUnavailable {
		t.Errorf("an unreachable provider answered %q, want %q", got, errProviderUnavailable)
	}
	if files := accountFiles(t, accountsDir); len(files) != 0 {
		t.Errorf("an unreachable provider left %d account records behind", len(files))
	}
}

// A code the provider will not redeem — this one because it was never issued — is the
// person's sign-in failing rather than the provider being down, and it says so.
func TestACodeTheProviderRefusesIsARefusalOfItsOwn(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	svc, accountsDir := signInService(t, fake, discard())

	begun := start(t, svc)

	rec := finish(t, svc, begun.State, "a code the provider never issued", begun.FinishSecret)
	if rec.Code != http.StatusBadRequest {
		t.Errorf("a refused code answered %d, want 400", rec.Code)
	}
	if got := refusalCode(t, rec); got != errProviderRefused {
		t.Errorf("a refused code answered %q, want %q", got, errProviderRefused)
	}
	if files := accountFiles(t, accountsDir); len(files) != 0 {
		t.Errorf("a refused code left %d account records behind", len(files))
	}
}

// A deployment with no Discord application answers, and says which flag is missing —
// rather than the routes being absent, which looks identical to a stale build.
func TestTheSignInRoutesSayWhenTheyAreNotConfigured(t *testing.T) {
	t.Parallel()

	svc := &service{log: discard()}

	for _, path := range []string{"/v1/signin/discord/start", "/v1/signin/discord/finish"} {
		rec := call(t, svc, http.MethodPost, path, `{"state":"a","code":"b"}`)
		if rec.Code != http.StatusServiceUnavailable {
			t.Errorf("POST %s answered %d on an unconfigured service, want 503", path, rec.Code)
		}
		if got := refusalCode(t, rec); got != errNotConfigured {
			t.Errorf("POST %s answered %q, want %q", path, got, errNotConfigured)
		}
	}
}

// An empty client id is "not configured" and not an error, so the account service still
// starts, still keeps accounts and still answers probes. A client id with a redirect URI
// that is not a URL is a real misconfiguration and refuses.
func TestNewSignInSeparatesUnconfiguredFromMisconfigured(t *testing.T) {
	t.Parallel()

	accounts, err := auth.OpenStore(filepath.Join(t.TempDir(), "auth"))
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}

	var out bytes.Buffer
	unconfigured, err := newSignIn(options{}, accounts, slog.New(slog.NewTextHandler(&out, nil)))
	if err != nil {
		t.Fatalf("an unconfigured service refused to start: %v", err)
	}
	if unconfigured != nil {
		t.Error("a service with no client id was given a sign-in flow")
	}
	if !strings.Contains(out.String(), "-discord-client-id") {
		t.Error("the startup log does not say which flag is missing")
	}

	misconfigured := options{discordClientID: "111", discordRedirectURI: "://not a url"}
	if _, err := newSignIn(misconfigured, accounts, discard()); err == nil {
		t.Error("a redirect URI that is not a URL was accepted")
	}
}

// A misconfigured sign-in refuses before the port is bound, in the order the store
// already does: the listen address this test holds is what makes the ordering readable,
// because reaching net.Listen would fail on the address instead.
func TestASignInThatCannotBeConfiguredRefusesBeforeThePortIsBound(t *testing.T) {
	t.Parallel()

	held, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listening: %v", err)
	}
	defer func() { _ = held.Close() }()

	opts := validOptions(t)
	opts.listen = held.Addr().String()
	opts.discordClientID = "111"
	opts.discordRedirectURI = "://not a url"

	err = run(context.Background(), opts, discard())
	if err == nil {
		t.Fatal("run started with a redirect URI that is not a URL")
	}
	if !strings.Contains(err.Error(), "configuring Discord sign-in") {
		t.Errorf("run failed with %q, which is not the sign-in refusing; the port was bound first", err)
	}
}

// **Nothing from the provider reaches the log.** A whole sign-in is captured through
// both handlers and every secret is looked for in every encoding a leak could take.
//
// Both handlers, because the JSON one is the one a Stringer would not have saved. The
// email is in the list even though this service never asks for the scope that returns
// one and never decodes the field: the fake sends it anyway, which is what makes the
// assertion about what this service does rather than about what it was sent.
//
// The Discord user id is in the list too. It is not a credential — knowing somebody's
// Discord id is not a way to become them — but it is personal data, and internal/auth
// keeps it out of its own errors for the same reason.
func TestNothingFromTheProviderReachesTheLog(t *testing.T) {
	t.Parallel()

	for name, build := range map[string]func(*bytes.Buffer) slog.Handler{
		"text": func(w *bytes.Buffer) slog.Handler {
			return slog.NewTextHandler(w, &slog.HandlerOptions{Level: slog.LevelDebug})
		},
		"json": func(w *bytes.Buffer) slog.Handler {
			return slog.NewJSONHandler(w, &slog.HandlerOptions{Level: slog.LevelDebug})
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			var out bytes.Buffer
			fake := newFakeDiscord(t)
			svc, _ := signInService(t, fake, slog.New(build(&out)))

			// A whole sign-in, and then every refusal a handler can log, so that the
			// paths where a value is most likely to be pasted into a message are all
			// exercised in one capture.
			begun := start(t, svc)
			code := fake.issue(t, begun.AuthorizeURL)
			if rec := finish(t, svc, begun.State, code, begun.FinishSecret); rec.Code != http.StatusOK {
				t.Fatalf("the sign-in answered %d: %s", rec.Code, rec.Body.String())
			}
			account := signInOnce(t, svc, fake)

			finish(t, svc, begun.State, code, begun.FinishSecret)            // a replayed sign-in
			finish(t, svc, "a state nobody minted", code, "a finish secret") // an unknown state
			call(t, svc, http.MethodPost, "/v1/signin/discord/finish",       // a body that is not JSON
				`{"state":"`+begun.State+`","code":"`+code+`"`)

			logged := out.String()
			if logged == "" {
				t.Fatal("the sign-in logged nothing, so this test proves nothing")
			}

			for label, secret := range map[string]string{
				"the access token":       fixtureAccessToken,
				"the refresh token":      fixtureRefreshToken,
				"the email":              fixtureEmail,
				"the authorization code": code,
				"the state":              begun.State,
				"the Discord user id":    fixtureSubject,
				"the display name":       fixtureGlobalName,
			} {
				for encoding, rendered := range map[string]string{
					"raw":       secret,
					"hex":       hex.EncodeToString([]byte(secret)),
					"base64":    base64.StdEncoding.EncodeToString([]byte(secret)),
					"base64url": base64.RawURLEncoding.EncodeToString([]byte(secret)),
				} {
					if strings.Contains(logged, rendered) {
						t.Errorf("%s appears in the log as %s", label, encoding)
					}
				}
			}

			// The lines that are supposed to be there, naming the account rather than
			// the person: an account id is minted at random and derived from nothing
			// about them.
			if !strings.Contains(logged, "sign-in completed") {
				t.Error("the log has no line for a completed sign-in")
			}
			if !strings.Contains(logged, account.AccountID) {
				t.Error("the log does not name the account the sign-in settled on")
			}
		})
	}
}
