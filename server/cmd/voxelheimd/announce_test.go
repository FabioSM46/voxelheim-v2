package main

import (
	"context"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/certs"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The values these tests announce.
//
// **Reserved documentation addresses, never a real one.** RFC 5737 sets 192.0.2.0/24 and
// 198.51.100.0/24 aside precisely so that an example cannot be somebody's machine, and this
// repository is public: an announcement is a home address, which is the one value the whole
// feature exists to keep current and the last one to write into a fixture.
const (
	testAnnounceAddress = "192.0.2.10:7777"
	otherTestAddress    = "198.51.100.7:7777"

	// testRegistrationKey is the credential these tests look for in captured logs. Long
	// enough to be one the account service would accept, and obviously not anybody's.
	testRegistrationKey = "registration-key-for-tests-0123456789abcdef"
)

// ---------------------------------------------------------------------------
// A fake registry: the account service's POST /v1/servers and nothing else of it
// ---------------------------------------------------------------------------

// fakeRegistry stands up the one endpoint an announcer talks to, and records what reaches
// it.
//
// **The bodies are handwritten and the fields are read by their literal names**, which is
// tickets_test.go's rule for the same reason: the game server and the account service are two
// programs that meet over HTTP, so what has to be pinned is that this side writes the names
// that side reads. Decoding into this package's own `announcement` struct would agree with a
// wrong spelling and hide the break behind a compile that still passes.
type fakeRegistry struct {
	mu        sync.Mutex
	received  []map[string]any
	presented []string

	// What it answers. A zero status is 200 with a well-formed acknowledgement.
	status int
	body   string

	// hold blocks every request until it is closed: the shape of a service that accepts a
	// connection and then says nothing.
	hold chan struct{}

	srv *httptest.Server
}

func newFakeRegistry(t *testing.T) *fakeRegistry {
	t.Helper()

	f := &fakeRegistry{}
	mux := http.NewServeMux()
	mux.HandleFunc("POST "+serversPath, f.handle)
	// The ticket key, so that a test can point the whole of `run` at this one service: a
	// game server reads its verifying key here at startup, and that read is deliberately
	// fatal while an announce deliberately is not.
	mux.HandleFunc("GET "+ticketKeyPath, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(publishedKey()))
	})

	// TLS with a certificate of its own, because there is no plaintext way to reach an
	// account service any more and two fakes have to be distinguishable to a pin (#131).
	f.srv = startTLS(t, mux)
	return f
}

// pin is the fingerprint -account-service-fingerprint must carry for this fake.
func (f *fakeRegistry) pin(t *testing.T) string {
	t.Helper()
	return fingerprintOf(t, f.srv)
}

func (f *fakeRegistry) handle(w http.ResponseWriter, r *http.Request) {
	raw, err := io.ReadAll(io.LimitReader(r.Body, 1<<16))
	if err != nil {
		raw = nil
	}
	var got map[string]any
	_ = json.Unmarshal(raw, &got)

	// Recorded before the hold below, deliberately: a test about a service that stalls has to
	// be able to see that the request arrived, and a record taken after the release would only
	// ever appear once the stall was over.
	f.mu.Lock()
	f.received = append(f.received, got)
	f.presented = append(f.presented, r.Header.Get("Authorization"))
	status, body := f.status, f.body
	f.mu.Unlock()

	if hold := f.holding(); hold != nil {
		select {
		case <-hold:
		case <-r.Context().Done():
			return
		}
	}

	if status == 0 {
		status, body = http.StatusOK, fmt.Sprintf(
			`{"name":%q,"created":true,"offline_after_seconds":300}`, testWorldName)
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = w.Write([]byte(body))
}

func (f *fakeRegistry) holding() chan struct{} {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.hold
}

// answer makes every later request answer status with body.
func (f *fakeRegistry) answer(status int, body string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.status, f.body = status, body
}

// stall makes every request block until the returned release is called.
func (f *fakeRegistry) stall() (release func()) {
	hold := make(chan struct{})
	f.mu.Lock()
	f.hold = hold
	f.mu.Unlock()

	var once sync.Once
	return func() { once.Do(func() { close(hold) }) }
}

func (f *fakeRegistry) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.received)
}

// last is the most recent announcement, and whether there is one.
func (f *fakeRegistry) last() (map[string]any, string, bool) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.received) == 0 {
		return nil, "", false
	}
	return f.received[len(f.received)-1], f.presented[len(f.presented)-1], true
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// withoutRegistrationKeyEnv takes the registration key out of the environment for one test
// and puts back whatever this machine had.
//
// t.Setenv is what registers the restore — including restoring "it was not set at all" — so
// unsetting after it is safe and is the only way to test the absent case on a machine that
// happens to export one.
func withoutRegistrationKeyEnv(t *testing.T) {
	t.Helper()

	t.Setenv(registrationKeyEnv, "placeholder")
	if err := os.Unsetenv(registrationKeyEnv); err != nil {
		t.Fatalf("Unsetenv: %v", err)
	}
}

// announceOptions is a configuration that announces: an account service to announce to, an
// address to announce, and a world to announce it under. The key comes from the environment,
// because it never comes from a flag.
// announceOptions is a configuration that announces to service.
//
// `servicePrint` is the account service's own certificate fingerprint, which is a
// different number from the `fingerprint` a test hands testAnnouncer below: that one is
// what this game server announces about *itself*. Two digests, two directions, and the
// parameter names are what keeps them apart.
func announceOptions(t *testing.T, service, servicePrint string) options {
	t.Helper()

	t.Setenv(registrationKeyEnv, testRegistrationKey)

	opts := validOptions()
	opts.ticketKey = ""
	opts.accountService = service
	opts.accountServiceFingerprint = servicePrint
	opts.announceAddress = testAnnounceAddress
	return opts
}

// testAnnouncer is an announcer pointed at service, with the intervals a test can wait on.
func testAnnouncer(t *testing.T, service, servicePrint, fingerprint string, log *slog.Logger) *announcer {
	t.Helper()

	a := newAnnouncer(announceOptions(t, service, servicePrint), fingerprint, log)
	if a == nil {
		t.Fatal("newAnnouncer refused a configuration that names a service, a key and an address")
	}
	a.every = 20 * time.Millisecond
	a.timeout = 2 * time.Second
	return a
}

// captured is a logger writing into a buffer, through the handler named. Both handlers,
// because a value that redacts through one may not redact through the other: slog resolves a
// LogValuer before formatting, and the JSON handler would otherwise hand a string straight to
// encoding/json.
type captured struct {
	mu   sync.Mutex
	text strings.Builder
}

func (c *captured) Write(p []byte) (int, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.text.Write(p)
}

func (c *captured) String() string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.text.String()
}

func capturingLogger(format string) (*slog.Logger, *captured) {
	sink := &captured{}
	opts := &slog.HandlerOptions{Level: slog.LevelDebug}
	if format == "json" {
		return slog.New(slog.NewJSONHandler(sink, opts)), sink
	}
	return slog.New(slog.NewTextHandler(sink, opts)), sink
}

// listenFor is the transport and the fingerprint of the certificate under dir.
func listenFor(t *testing.T, dir string) (string, func()) {
	t.Helper()

	tr, fingerprint, err := listen(options{listen: "127.0.0.1:0", worldDir: dir}, discard())
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	return fingerprint, func() { _ = tr.Close() }
}

// ---------------------------------------------------------------------------
// What is announced
// ---------------------------------------------------------------------------

// The fingerprint that goes to the list is the one the certificate actually has.
//
// **There is one source for that number and this is what says so.** #150 made a client take
// its expectation from the list rather than from a pinned file, so a fingerprint announced
// from any second computation is a server every client refuses — and the failure looks like a
// networking bug rather than like a wrong digest. The comparison here is against
// `certs.Fingerprint` read straight off the stored certificate, which is the only sanctioned
// way to obtain it.
func TestAnAnnouncementCarriesTheFingerprintTheCertificateActuallyHas(t *testing.T) {
	dir := t.TempDir()
	fingerprint, closeListener := listenFor(t, dir)
	defer closeListener()

	registry := newFakeRegistry(t)
	a := testAnnouncer(t, registry.srv.URL, registry.pin(t), fingerprint, discard())
	a.announce(context.Background())

	got, presented, ok := registry.last()
	if !ok {
		t.Fatal("the announcer sent nothing")
	}

	cert, err := certs.LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	want, err := certs.Fingerprint(cert)
	if err != nil {
		t.Fatalf("Fingerprint: %v", err)
	}

	// Read by the literal names the account service's handler reads, never through this
	// package's own struct: a renamed field must fail here rather than agree with itself.
	if got["certificate_sha256"] != want {
		t.Errorf("announced certificate_sha256 %v, want the certificate's own %s", got["certificate_sha256"], want)
	}
	if got["name"] != testWorldName {
		t.Errorf("announced name %v, want %s", got["name"], testWorldName)
	}
	if got["address"] != testAnnounceAddress {
		t.Errorf("announced address %v, want the configured one", got["address"])
	}
	if presented != "Bearer "+testRegistrationKey {
		t.Error("the registration key was not presented as a bearer credential")
	}
}

// The announced address is the operator's, not the listener's.
//
// A server bound to 127.0.0.1 on a free port announcing that would be a row in the list every
// player dials and none reaches. The two values are separate on purpose, and this is the test
// that keeps them separate.
func TestTheAnnouncedAddressIsHonouredOverTheListenAddress(t *testing.T) {
	dir := t.TempDir()

	tr, fingerprint, err := listen(options{listen: "127.0.0.1:0", worldDir: dir}, discard())
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer func() { _ = tr.Close() }()

	registry := newFakeRegistry(t)
	opts := announceOptions(t, registry.srv.URL, registry.pin(t))
	opts.listen = tr.Addr()
	opts.announceAddress = otherTestAddress

	a := newAnnouncer(opts, fingerprint, discard())
	if a == nil {
		t.Fatal("newAnnouncer refused a configuration that names a service, a key and an address")
	}
	a.announce(context.Background())

	got, _, ok := registry.last()
	if !ok {
		t.Fatal("the announcer sent nothing")
	}
	if got["address"] != otherTestAddress {
		t.Errorf("announced %v, want the -announce-address value %s", got["address"], otherTestAddress)
	}
	if got["address"] == tr.Addr() {
		t.Errorf("the listen address %s was announced; a server on 0.0.0.0 would announce something unreachable", tr.Addr())
	}
}

// The repeat is what makes an address that changes while the server is up reach the list
// without a restart, so the loop has to fire more than once on its own.
func TestTheAnnounceRepeatsOnItsInterval(t *testing.T) {
	registry := newFakeRegistry(t)
	a := testAnnouncer(t, registry.srv.URL, registry.pin(t), strings.Repeat("ab", 32), discard())

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- a.loop(ctx) }()

	deadline := time.Now().Add(10 * time.Second)
	for registry.count() < 3 {
		if time.Now().After(deadline) {
			t.Fatalf("the announcer sent %d announcements in ten seconds; the repeat is not firing", registry.count())
		}
		time.Sleep(5 * time.Millisecond)
	}

	cancel()
	select {
	case err := <-done:
		if err == nil {
			t.Error("the loop returned nil on cancellation; it is a worker and shutdown waits on it")
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the announce loop did not end when its context was cancelled")
	}
}

// ---------------------------------------------------------------------------
// The criterion the whole issue is for: a failed announce is never fatal
// ---------------------------------------------------------------------------

// **A server whose announcements fail is a server that is still serving players.**
//
// This is the acceptance criterion the rest of the design rests on. Admitting a player is a
// signature check precisely so that the account service being down costs nobody a game; an
// announcer that could take the server down over a call to that same service would undo it in
// one line. So the three ways that call goes wrong are driven through a real server with a
// real handshake, and what is asserted is that the player is welcomed anyway.
//
// The three shapes are deliberately different failures rather than three spellings of one:
// nobody listening at all, a service that answers a refusal, and one that accepts the
// connection and then says nothing. The last is the one a timeout is the only defence
// against — without it the worker sits on a socket for the life of the process.
func TestAFailedAnnounceIsLoggedAndSurvived(t *testing.T) {
	shapes := map[string]func(t *testing.T) (service, servicePrint string, cleanup func()){
		"nobody is listening": func(t *testing.T) (string, string, func()) {
			// Gone rather than slow: stood up so the URL is a real one, then closed with its
			// listener released.
			registry := newFakeRegistry(t)
			url, pin := registry.srv.URL, registry.pin(t)
			registry.srv.Close()
			return url, pin, func() {}
		},
		"the service answers a refusal": func(t *testing.T) (string, string, func()) {
			registry := newFakeRegistry(t)
			registry.answer(http.StatusServiceUnavailable, `{"error":"registry_unavailable"}`)
			return registry.srv.URL, registry.pin(t), func() {}
		},
		"the service stalls": func(t *testing.T) (string, string, func()) {
			registry := newFakeRegistry(t)
			release := registry.stall()
			return registry.srv.URL, registry.pin(t), release
		},
		"the service answers nonsense": func(t *testing.T) (string, string, func()) {
			registry := newFakeRegistry(t)
			registry.answer(http.StatusOK, "this is not JSON, and it is not an acknowledgement either")
			return registry.srv.URL, registry.pin(t), func() {}
		},
		// **A service answering for the address that is not the one pinned**, which is
		// the shape #131 closed and the one an announcer has to survive like any other:
		// the registration key travels in that request, so a handshake that failed is a
		// credential that was not presented. It costs no player anything — the game runs
		// and this server simply does not appear in the list.
		"the service presents a certificate nobody pinned": func(t *testing.T) (string, string, func()) {
			registry := newFakeRegistry(t)
			return registry.srv.URL, strings.Repeat("ab", 32), func() {}
		},
	}

	for name, stand := range shapes {
		t.Run(name, func(t *testing.T) {
			service, servicePrint, cleanup := stand(t)
			defer cleanup()

			log, logged := capturingLogger("text")
			a := testAnnouncer(t, service, servicePrint, strings.Repeat("cd", 32), log)
			// Short enough that a stall costs a test a moment rather than the whole budget,
			// and long enough that a loopback request is not raced by its own deadline.
			a.timeout = 150 * time.Millisecond

			conn := newScriptedConn(name)
			srv := newTestServer(t, newQueueTransport(conn), world.NewCache(testConfig().WorldSeed, 1, 64), nil)
			srv.announce = a

			stop := start(t, srv)
			defer stop()

			// **The whole assertion.** A player presents a ticket while every announcement
			// this server makes is failing, and is welcomed.
			if got := enterWorld(t, conn, helloFor(t, testAccount(11)), creationOf("Eivor")).PayloadType(); got != vnet.PayloadServerWelcome {
				t.Fatalf("the session got %s while announcing was failing, want a welcome", got)
			}

			// And the failure was said out loud rather than swallowed. Waited for rather than
			// asserted at once: the first announcement and the handshake are two goroutines.
			deadline := time.Now().Add(10 * time.Second)
			for !strings.Contains(logged.String(), "announcing this server failed") {
				if time.Now().After(deadline) {
					t.Fatalf("a failing announce was never logged; the log said: %s", logged.String())
				}
				time.Sleep(5 * time.Millisecond)
			}
			if !strings.Contains(logged.String(), "keeps serving") {
				t.Error("the failure line does not say the server is still serving, which is the thing its reader needs to know")
			}
		})
	}
}

// A service that has been down since lunchtime is one thing that is wrong, not three hundred.
//
// The acceptance criterion is that a failed announce is logged; the criterion beside it is
// that the log does not become a complaint per interval, because a warning that repeats on a
// timer stops being read. So the first failure warns, an identical repeat is a debug line, and
// a change of reason warns again — a service coming back up and refusing the key is a second
// thing, not the same thing.
func TestAPersistentFailureIsOneWarningRatherThanOnePerInterval(t *testing.T) {
	registry := newFakeRegistry(t)
	registry.answer(http.StatusServiceUnavailable, `{"error":"registry_unavailable"}`)

	log, logged := capturingLogger("text")
	a := testAnnouncer(t, registry.srv.URL, registry.pin(t), strings.Repeat("ef", 32), log)

	for range 5 {
		a.announce(context.Background())
	}
	if got := strings.Count(logged.String(), "level=WARN"); got != 1 {
		t.Errorf("five identical failures produced %d warnings, want exactly one:\n%s", got, logged.String())
	}
	if got := strings.Count(logged.String(), "announcing this server failed"); got != 5 {
		t.Errorf("five failures produced %d lines; every failed announce is logged, loudly or not", got)
	}

	// A different reason is a different thing to say.
	registry.answer(http.StatusUnauthorized, `{"error":"unauthorized"}`)
	a.announce(context.Background())
	if got := strings.Count(logged.String(), "level=WARN"); got != 2 {
		t.Errorf("a changed failure produced %d warnings in total, want two:\n%s", got, logged.String())
	}

	// And coming back is worth saying once.
	registry.answer(0, "")
	a.announce(context.Background())
	a.announce(context.Background())
	if got := strings.Count(logged.String(), "this server is in the account service's list"); got != 1 {
		t.Errorf("recovery was announced %d times, want once:\n%s", got, logged.String())
	}
}

// A server nobody asked to announce says so once and then never again.
//
// The other half of the rule above, and the one the acceptance criteria state outright: not
// announcing is an ordinary way to run a server, so it is a clean single line rather than a
// warning per interval.
func TestAServerWithNothingConfiguredAnnouncesNothingAndSaysSoOnce(t *testing.T) {
	withoutRegistrationKeyEnv(t)

	log, logged := capturingLogger("text")
	opts := validOptions()

	a := newAnnouncer(opts, strings.Repeat("ab", 32), log)
	if a != nil {
		t.Fatal("a server with no account service, no key and no announce address built an announcer")
	}
	if got := strings.Count(logged.String(), "\n"); got != 1 {
		t.Errorf("not announcing cost %d log lines, want exactly one:\n%s", got, logged.String())
	}
	if !strings.Contains(logged.String(), "level=INFO") {
		t.Errorf("not announcing was reported at a level other than INFO:\n%s", logged.String())
	}

	// And the loop over it is a no-op: no request, no line, no goroutine left running.
	before := logged.String()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := a.loop(ctx); err != nil {
		t.Errorf("a nil announcer's loop returned %v, want nil", err)
	}
	if logged.String() != before {
		t.Errorf("a nil announcer's loop wrote to the log:\n%s", logged.String())
	}
}

// An exported-but-empty registration key is "not given", not "given as nothing".
//
// **This is the state `.env.example` produces**, and it is the reason the distinction is
// worth a test: sourcing that file exports every name in it — each assignment carries its
// own `export` — and a run that wants no server list leaves this one empty. Reading mere presence made that start report
// itself as half-configured — a warning saying the game runs but will not appear in a list —
// where the truth is that nobody asked it to announce at all.
func TestAnEmptyRegistrationKeyVariableIsNotAConfiguration(t *testing.T) {
	t.Setenv(registrationKeyEnv, "")

	log, logged := capturingLogger("text")
	if a := newAnnouncer(validOptions(), strings.Repeat("ab", 32), log); a != nil {
		t.Fatal("an empty registration key built an announcer")
	}
	if strings.Contains(logged.String(), "level=WARN") {
		t.Errorf("an empty registration key was reported as a misconfiguration:\n%s", logged.String())
	}
	if !strings.Contains(logged.String(), "level=INFO") {
		t.Errorf("not announcing was not reported at INFO:\n%s", logged.String())
	}

	// And it is the *unset* answer rather than an error, read at the source.
	key, err := registrationKeyFor("")
	if err != nil {
		t.Fatalf("an empty registration key variable was an error: %v", err)
	}
	if key != "" {
		t.Errorf("an empty registration key variable produced %q, want no key", string(key))
	}

	// The file beside it still works while the variable is exported empty — which is the
	// half that would have been refused as "given in both places" had presence been the test.
	path := filepath.Join(t.TempDir(), "registration-key")
	if err := os.WriteFile(path, []byte(testRegistrationKey+"\n"), 0o600); err != nil {
		t.Fatalf("writing the key file: %v", err)
	}
	fromFile, err := registrationKeyFor(path)
	if err != nil {
		t.Fatalf("reading the key from a file beside an empty variable: %v", err)
	}
	if string(fromFile) != testRegistrationKey {
		t.Errorf("the key read from a file is %q, want the one written", string(fromFile))
	}
}

// ---------------------------------------------------------------------------
// What a configuration has to say before anything is announced
// ---------------------------------------------------------------------------

// Every one of these is a configuration that cannot announce, and not one of them is a reason
// to refuse a start.
//
// **The direction that matters is the fatal one.** Any of these could have been an error
// returned from run — the flags are checked at startup and a bad one refuses a start
// everywhere else in this file — and that would make an optional side channel a hard
// dependency of running a game, which is the whole thing the issue exists to prevent.
func TestAnUnusableAnnounceConfigurationDisablesAnnouncingRatherThanTheServer(t *testing.T) {
	cases := map[string]func(t *testing.T, o *options){
		"an announce address that is not host:port": func(t *testing.T, o *options) {
			o.announceAddress = "example.invalid"
		},
		"an announce address with no host": func(t *testing.T, o *options) {
			o.announceAddress = ":7777"
		},
		"an announce address on every interface": func(t *testing.T, o *options) {
			o.announceAddress = "0.0.0.0:7777"
		},
		"an announce address on every interface, in v6": func(t *testing.T, o *options) {
			o.announceAddress = "[::]:7777"
		},
		"an announce address with a named port": func(t *testing.T, o *options) {
			o.announceAddress = "example.invalid:http"
		},
		"an announce address with a port of zero": func(t *testing.T, o *options) {
			o.announceAddress = "192.0.2.10:0"
		},
		"an announce address with a port past a uint16": func(t *testing.T, o *options) {
			o.announceAddress = "192.0.2.10:99999"
		},
		"a key that cannot be presented in a header": func(t *testing.T, o *options) {
			t.Setenv(registrationKeyEnv, "a key with spaces in it and more besides")
		},
		"an empty key": func(t *testing.T, o *options) {
			t.Setenv(registrationKeyEnv, "")
		},
		"a key from two sources at once": func(t *testing.T, o *options) {
			o.registrationKeyFile = writeKeyFile(t, testRegistrationKey)
		},
		"a key file that is not there": func(t *testing.T, o *options) {
			withoutRegistrationKeyEnv(t)
			o.registrationKeyFile = t.TempDir() + "/no-such-key"
		},
		"a key and an address but no account service": func(t *testing.T, o *options) {
			o.accountService = ""
		},
		"a key and a service but no announce address": func(t *testing.T, o *options) {
			o.announceAddress = ""
		},
		"an account service that is not a URL": func(t *testing.T, o *options) {
			o.accountService = "not://a service"
		},
	}

	for name, mutate := range cases {
		t.Run(name, func(t *testing.T) {
			opts := announceOptions(t, "https://127.0.0.1:1", strings.Repeat("00", 32))
			mutate(t, &opts)

			log, logged := capturingLogger("text")
			if a := newAnnouncer(opts, strings.Repeat("ab", 32), log); a != nil {
				t.Fatalf("%s built an announcer", name)
			}
			if got := strings.Count(logged.String(), "\n"); got != 1 {
				t.Errorf("%s cost %d log lines, want exactly one:\n%s", name, got, logged.String())
			}
			// Louder than "nobody asked", because somebody meant to be in the list and will
			// not be — and still not a refusal to start.
			if !strings.Contains(logged.String(), "level=WARN") {
				t.Errorf("%s was not warned about:\n%s", name, logged.String())
			}
		})
	}
}

// The same claim one level up, through the function that actually starts a server: a
// configuration that cannot announce still binds a port, opens a world and shuts down cleanly.
//
// The context is cancelled before run is called, so what this exercises is the whole of
// startup — the ticket key fetch, the world, the listener, the announcer — and then an
// immediate shutdown. A nil error is the assertion.
func TestABrokenAnnounceConfigurationDoesNotRefuseTheStart(t *testing.T) {
	registry := newFakeRegistry(t)

	opts := announceOptions(t, registry.srv.URL, registry.pin(t))
	opts.worldDir = t.TempDir()
	opts.announceAddress = "0.0.0.0:7777" // every interface: refused by the announcer
	opts.logLevel = "info"

	log, logged := capturingLogger("text")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := make(chan error, 1)
	go func() { done <- run(ctx, opts, log) }()

	deadline := time.Now().Add(30 * time.Second)
	for !strings.Contains(logged.String(), "voxelheimd listening") {
		select {
		case err := <-done:
			t.Fatalf("run refused to start over an announce configuration it could not use: %v", err)
		default:
		}
		if time.Now().After(deadline) {
			t.Fatalf("the server never came up:\n%s", logged.String())
		}
		time.Sleep(5 * time.Millisecond)
	}

	if !strings.Contains(logged.String(), "will not announce itself") {
		t.Errorf("the start said nothing about not announcing:\n%s", logged.String())
	}

	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("run ended with %v", err)
		}
	case <-time.After(30 * time.Second):
		t.Fatal("the server did not shut down")
	}
}

// writeKeyFile puts a registration key in a file and answers its path.
func writeKeyFile(t *testing.T, key string) string {
	t.Helper()

	path := t.TempDir() + "/registration-key"
	// A trailing newline on purpose: `echo key > key-file` leaves one, and an operator who
	// had to notice that would notice it as a 401 with nothing in any log to explain it.
	if err := os.WriteFile(path, []byte(key+"\n"), 0o600); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}
	return path
}

// A key in a file is a key, newline and all — the trim is what makes `echo key > key-file`
// work, and both ends of this secret do it.
func TestAKeyIsReadFromAFileAndTrimmed(t *testing.T) {
	withoutRegistrationKeyEnv(t)

	registry := newFakeRegistry(t)
	opts := validOptions()
	opts.ticketKey = ""
	opts.accountService = registry.srv.URL
	opts.accountServiceFingerprint = registry.pin(t)
	opts.announceAddress = testAnnounceAddress
	opts.registrationKeyFile = writeKeyFile(t, testRegistrationKey)

	a := newAnnouncer(opts, strings.Repeat("ab", 32), discard())
	if a == nil {
		t.Fatal("a key in a file did not configure an announcer")
	}
	a.announce(context.Background())

	_, presented, ok := registry.last()
	if !ok {
		t.Fatal("the announcer sent nothing")
	}
	if presented != "Bearer "+testRegistrationKey {
		t.Error("the key read from the file was not the key presented")
	}
}

// ---------------------------------------------------------------------------
// The credential
// ---------------------------------------------------------------------------

// **The registration key never reaches a log**, and this drives a whole announce plus every
// refusal through both handlers to say so.
//
// It is not a player credential; it is the credential that decides who may put an address in
// the list under a name players trust, which makes a leaked one a better attack than the one
// the list replaces. The search is for the key in raw, hex and base64 — a secrecy test
// looking for the wrong encoding passes while proving nothing — and through both the text and
// the JSON handler, because they are different routes: slog resolves a LogValuer before either
// formats, and without one the JSON handler would hand the string to encoding/json.
func TestTheRegistrationKeyNeverReachesTheLog(t *testing.T) {
	for _, format := range []string{"text", "json"} {
		t.Run(format, func(t *testing.T) {
			registry := newFakeRegistry(t)
			log, logged := capturingLogger(format)

			a := testAnnouncer(t, registry.srv.URL, registry.pin(t), strings.Repeat("ab", 32), log)

			// A success, then every shape of refusal, then a service that is gone: the paths
			// an operator actually investigates are the ones a secret leaks on.
			a.announce(context.Background())
			for _, answer := range []struct {
				status int
				body   string
			}{
				{http.StatusUnauthorized, `{"error":"unauthorized"}`},
				{http.StatusBadRequest, `{"error":"address_refused"}`},
				{http.StatusServiceUnavailable, `{"error":"registration_not_configured"}`},
				{http.StatusInternalServerError, `{"error":"` + testRegistrationKey + `"}`},
				{http.StatusOK, `{"name":"` + testRegistrationKey + `"}`},
				{http.StatusTeapot, testRegistrationKey},
			} {
				registry.answer(answer.status, answer.body)
				a.announce(context.Background())
			}
			registry.srv.Close()
			a.announce(context.Background())

			// And the value itself, through every route out of the type that is not the one
			// deliberate reveal.
			// The key on its own, and — the route it actually leaked by — the struct that
			// holds it, as a pointer and as a value, through every verb that walks fields.
			log.Info("the announcer", "announcer", a, "value", *a, "key", a.key)
			// Every verb in one format string apiece: %v, %s and %q reach a Stringer, %#v
			// reaches a GoStringer and nothing else, and %+v is the walker that steps past an
			// unexported field. (One verb per Sprintf would be the shorter spelling and is
			// what S1025 asks for — the point here is precisely to call the ones a caller
			// might, so they are batched rather than replaced by String().)
			log.Info("formatted",
				"key", fmt.Sprintf("v=%v s=%s q=%q sharp=%#v", a.key, a.key, a.key, a.key),
				"struct", fmt.Sprintf("v=%v plus=%+v sharp=%#v", a, *a, *a))
			marshalled, err := json.Marshal(struct {
				Key registrationKey `json:"key"`
			}{a.key})
			if err != nil {
				t.Fatalf("Marshal: %v", err)
			}
			log.Info("marshalled", "json", string(marshalled))

			out := logged.String()
			if out == "" {
				t.Fatal("nothing was captured; this test is not looking at what it claims to")
			}
			for encoding, value := range map[string]string{
				"raw":       testRegistrationKey,
				"hex":       hex.EncodeToString([]byte(testRegistrationKey)),
				"base64":    base64.StdEncoding.EncodeToString([]byte(testRegistrationKey)),
				"base64url": base64.URLEncoding.EncodeToString([]byte(testRegistrationKey)),
			} {
				if strings.Contains(out, value) {
					t.Errorf("the %s registration key reached the %s log", encoding, format)
				}
			}
			if strings.Contains(string(marshalled), testRegistrationKey) {
				t.Error("a struct holding the key marshalled it verbatim")
			}
		})
	}
}

// The announced address is the one value this server deliberately keeps out of its own log —
// internal/registry's rule for the same string, and for its reason: it locates somebody's
// house, which is why the list is behind a credential at all.
func TestTheAnnouncedAddressIsNotLogged(t *testing.T) {
	registry := newFakeRegistry(t)
	log, logged := capturingLogger("text")

	a := testAnnouncer(t, registry.srv.URL, registry.pin(t), strings.Repeat("ab", 32), log)
	a.announce(context.Background())
	registry.answer(http.StatusBadRequest, `{"error":"address_refused"}`)
	a.announce(context.Background())
	registry.srv.Close()
	a.announce(context.Background())

	// And through the struct that holds it, which is the route the registration key actually
	// leaked by while this was being written: an unexported field is invisible to fmt's
	// Stringer lookup, so the outer type has to redact.
	log.Info("the announcer", "announcer", a, "value", *a)
	log.Info("formatted", "struct", fmt.Sprintf("v=%v plus=%+v sharp=%#v", a, *a, *a))

	if strings.Contains(logged.String(), testAnnounceAddress) {
		t.Errorf("the announced address reached the log:\n%s", logged.String())
	}
}

// ---------------------------------------------------------------------------
// The interval, taken from the acknowledgement rather than copied
// ---------------------------------------------------------------------------

// internal/registry.OfflineAfter is documented as the number the announcing side must be
// under, and this process may not import that package — the boundary test in
// internal/registry says so. The account service publishes it in every acknowledgement
// instead, and this is the arithmetic that believes it.
//
// The direction is the point: it only ever narrows. A window this announcer is already well
// inside changes nothing, and no answer can slow it down or turn it into a hot loop.
func TestTheIntervalIsTakenFromTheAcknowledgement(t *testing.T) {
	cases := []struct {
		name       string
		configured time.Duration
		published  int64
		want       time.Duration
	}{
		{"the documented window leaves a minute alone", time.Minute, 300, time.Minute},
		{"a tighter window narrows the interval", time.Minute, 120, 30 * time.Second},
		{"a wider window cannot slow it down", time.Minute, 3600, time.Minute},
		{"a tiny window cannot make a hot loop", time.Minute, 4, minAnnounceEvery},
		{"an absent field changes nothing", time.Minute, 0, time.Minute},
		{"a negative window changes nothing", time.Minute, -1, time.Minute},
		{"an absurd window changes nothing", time.Minute, maxOfflineAfterSeconds + 1, time.Minute},
		{"a shortened test interval is never lengthened", 20 * time.Millisecond, 300, 20 * time.Millisecond},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			a := &announcer{every: tc.configured}
			if got := a.settle(tc.published); got != tc.want {
				t.Errorf("settle(%d) with %s configured answered %s, want %s", tc.published, tc.configured, got, tc.want)
			}
			if a.every != tc.want {
				t.Errorf("settle left the interval at %s, want %s", a.every, tc.want)
			}
		})
	}
}

// A stalled announce ends when the server does, rather than holding a socket for the life of
// the process. The context the request runs under is the shutdown one, so cancelling it is
// what has to unblock the worker.
func TestAStalledAnnounceEndsWithTheServer(t *testing.T) {
	registry := newFakeRegistry(t)
	release := registry.stall()
	defer release()

	a := testAnnouncer(t, registry.srv.URL, registry.pin(t), strings.Repeat("ab", 32), discard())
	// Far longer than this test is allowed to take: what must end the announce is the
	// cancellation, not the deadline.
	a.timeout = time.Hour

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- a.loop(ctx) }()

	// Wait until the request is actually in flight, so the cancellation lands mid-stall
	// rather than before it.
	deadline := time.Now().Add(10 * time.Second)
	for registry.count() == 0 {
		if time.Now().After(deadline) {
			t.Fatal("the announcer made no request to stall")
		}
		time.Sleep(5 * time.Millisecond)
	}

	cancel()
	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatal("a stalled announce outlived the shutdown that cancelled it")
	}
}
