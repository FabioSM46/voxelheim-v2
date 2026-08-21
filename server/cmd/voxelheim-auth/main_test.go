package main

import (
	"bytes"
	"context"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/auth"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

func discard() *slog.Logger { return slog.New(slog.DiscardHandler) }

// newKeys is a ticket signing pair in a directory belonging to this test.
//
// Generated at run time, never committed: a key pair checked into a public repository is
// a key pair somebody eventually signs a real ticket with.
//
// Every service these tests build gets one, because a service without one is a state
// that cannot exist — run returns before constructing the service if the pair cannot be
// read, which is what lets the handlers dereference it without a guard.
func newKeys(t *testing.T) *ticket.Pair {
	t.Helper()

	keys, err := ticket.LoadOrCreate(t.TempDir())
	if err != nil {
		t.Fatalf("ticket.LoadOrCreate: %v", err)
	}
	return keys
}

// validOptions is a configuration every field of which passes validate. The cases
// below mutate the single field under test rather than building a literal each time:
// a literal that omits a field is a case that passes for a reason it did not mean.
func validOptions(t *testing.T) options {
	t.Helper()
	return options{
		listen:    "127.0.0.1:0",
		authDir:   filepath.Join(t.TempDir(), "auth"),
		logLevel:  "info",
		logFormat: "text",
	}
}

func TestOptionsValidate(t *testing.T) {
	t.Parallel()

	if err := validOptions(t).validate(); err != nil {
		t.Fatalf("valid flags rejected: %v", err)
	}

	// The raw value is what gets validated. A clamped check would accept every one of
	// these and start a service the operator did not ask for — see the port cases,
	// where narrowing first turns 99999 into a port nobody typed.
	cases := map[string]func(*options){
		"no accounts directory":                func(o *options) { o.authDir = "" },
		"no listen address":                    func(o *options) { o.listen = "" },
		"a listen address with no port":        func(o *options) { o.listen = "127.0.0.1" },
		"a listen address that is a bare port": func(o *options) { o.listen = "7778" },
		"a named port":                         func(o *options) { o.listen = "127.0.0.1:http" },
		"a port past a uint16":                 func(o *options) { o.listen = "127.0.0.1:65536" },
		"a port far past a uint16":             func(o *options) { o.listen = "127.0.0.1:99999" },
		"a negative port":                      func(o *options) { o.listen = "127.0.0.1:-1" },
	}
	for name, break_ := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			opts := validOptions(t)
			break_(&opts)
			if err := opts.validate(); err == nil {
				t.Error("the flags were accepted")
			}
		})
	}
}

// The error quotes what the operator typed, which is the whole point of validating
// before narrowing: a message naming 33465 for a `-listen :99999` describes a number
// nobody entered and sends the reader looking in the wrong place.
func TestOptionsValidateReportsTheValueGiven(t *testing.T) {
	t.Parallel()

	opts := validOptions(t)
	opts.listen = "127.0.0.1:99999"

	err := opts.validate()
	if err == nil {
		t.Fatal("a port past a uint16 was accepted")
	}
	if !strings.Contains(err.Error(), "99999") {
		t.Errorf("the refusal is %q, which does not quote the port the operator typed", err)
	}
}

// **The route table is the whole surface.** Every pattern it declares answers, and
// nothing it does not declare is reachable — including the pprof handlers, which
// would be there if this service had drifted onto http.DefaultServeMux.
func TestTheRouteTableIsTheWholeSurface(t *testing.T) {
	t.Parallel()

	svc := &service{log: discard(), keys: newKeys(t)}
	table := svc.routes()
	if len(table) == 0 {
		t.Fatal("the route table is empty; this test would pass by describing nothing")
	}
	mux := newMux(table)

	for _, r := range table {
		method, path, ok := strings.Cut(r.pattern, " ")
		if !ok {
			t.Fatalf("the route %q carries no method; the method belongs in the pattern", r.pattern)
		}
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, httptest.NewRequest(method, path, nil))
		if rec.Code == http.StatusNotFound {
			t.Errorf("the declared route %q answers 404", r.pattern)
		}
	}

	for _, path := range []string{"/", "/accounts", "/healthz/and-more", "/metrics", "/debug/pprof/"} {
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, path, nil))
		if rec.Code != http.StatusNotFound {
			t.Errorf("GET %s answered %d; something is reachable that the route table does not declare", path, rec.Code)
		}
	}
}

func TestHealthAnswersThatTheProcessIsUp(t *testing.T) {
	t.Parallel()

	rec := httptest.NewRecorder()
	newMux((&service{log: discard(), keys: newKeys(t)}).routes()).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/healthz", nil))

	if rec.Code != http.StatusOK {
		t.Errorf("GET /healthz answered %d, want 200", rec.Code)
	}
	if got := rec.Header().Get("Content-Type"); got != "application/json" {
		t.Errorf("GET /healthz answered with content type %q, want application/json", got)
	}
	if got, want := strings.TrimSpace(rec.Body.String()), `{"status":"ok"}`; got != want {
		t.Errorf("GET /healthz answered %q, want %q", got, want)
	}
}

// The method is part of the route, so a wrong one is a 405 from the mux rather than
// the first four lines of every handler. HEAD comes with GET, which is what a probe
// that only wants the status line uses.
func TestHealthAnswersTheMethodsItsPatternDeclares(t *testing.T) {
	t.Parallel()

	mux := newMux((&service{log: discard(), keys: newKeys(t)}).routes())

	for method, want := range map[string]int{
		http.MethodGet:    http.StatusOK,
		http.MethodHead:   http.StatusOK,
		http.MethodPost:   http.StatusMethodNotAllowed,
		http.MethodPut:    http.StatusMethodNotAllowed,
		http.MethodDelete: http.StatusMethodNotAllowed,
	} {
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, httptest.NewRequest(method, "/healthz", nil))
		if rec.Code != want {
			t.Errorf("%s /healthz answered %d, want %d", method, rec.Code, want)
		}
	}
}

// The serving path itself, over a listener the test owns: it answers while the
// context is live, and the context ending is what stops it — cleanly, and without
// leaving the port bound.
func TestServeAnswersUntilTheContextEnds(t *testing.T) {
	t.Parallel()

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listening: %v", err)
	}
	addr := ln.Addr().String()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	stopped := make(chan error, 1)
	go func() { stopped <- (&service{log: discard(), keys: newKeys(t)}).serve(ctx, ln) }()

	client := &http.Client{Timeout: 5 * time.Second}
	resp, err := client.Get("http://" + addr + "/healthz")
	if err != nil {
		t.Fatalf("GET /healthz: %v", err)
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != http.StatusOK {
		t.Errorf("GET /healthz answered %d, want 200", resp.StatusCode)
	}

	cancel()
	select {
	case err := <-stopped:
		if err != nil {
			t.Fatalf("serve returned %v, want a clean stop", err)
		}
	case <-time.After(30 * time.Second):
		t.Fatal("serve did not return after its context ended")
	}

	// Shutdown closes the listener, so nothing is left answering on the port.
	after, err := client.Get("http://" + addr + "/healthz")
	if err == nil {
		_ = after.Body.Close()
		t.Error("the service still answers after serve returned")
	}
}

func TestRunRefusesInvalidFlags(t *testing.T) {
	t.Parallel()

	opts := validOptions(t)
	opts.listen = "127.0.0.1:99999"

	if err := run(context.Background(), opts, discard()); err == nil {
		t.Fatal("run started with a port past a uint16")
	}
}

// **The store is opened before the listener is bound**, in the order cmd/voxelheimd
// opens its world in: the storage is the last thing that can refuse this
// configuration, and a service that has already bound a port and answered a probe is
// a worse place to discover its accounts directory cannot be created.
//
// The listen address is one this test is already holding, so the ordering is what the
// assertion reads: if run reached net.Listen it would fail on the address instead, and
// the error would say so.
func TestTheStoreIsOpenedBeforeThePortIsBound(t *testing.T) {
	t.Parallel()

	held, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listening: %v", err)
	}
	defer func() { _ = held.Close() }()

	blocked := filepath.Join(t.TempDir(), "blocked")
	if err := os.WriteFile(blocked, []byte("not a directory"), 0o600); err != nil {
		t.Fatalf("writing the blocking file: %v", err)
	}

	opts := validOptions(t)
	opts.listen = held.Addr().String()
	opts.authDir = blocked

	err = run(context.Background(), opts, discard())
	if err == nil {
		t.Fatal("run started with an accounts directory it cannot create")
	}
	if !strings.Contains(err.Error(), "opening the account store") {
		t.Errorf("run failed with %q, which is not the store refusing; the port was bound first", err)
	}
}

// The startup line an operator reads: which directory this service is keeping
// accounts in, and which format version it speaks. The shape cmd/voxelheimd already
// logs for its own stores.
func TestTheStartupLogNamesTheStoreAndItsFormatVersion(t *testing.T) {
	t.Parallel()

	var out bytes.Buffer
	log := slog.New(slog.NewTextHandler(&out, nil))

	// Already cancelled, so serve shuts down as soon as it starts: what is under test
	// is everything run does before it serves.
	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	opts := validOptions(t)
	if err := run(ctx, opts, log); err != nil {
		t.Fatalf("run: %v", err)
	}

	logged := out.String()
	for _, want := range []string{
		"account store opened",
		"accounts_dir=",
		fmt.Sprintf("format_version=%d", auth.StoreVersion),
		"voxelheim-auth listening",
		"addr=",
		// **The public key is logged deliberately**, because an operator has to read it
		// off this line in order to give it to a game server. It is the one half that
		// may be here; TestNothingOfTheSigningKeyReachesTheLog is the other side of
		// that sentence.
		"ticket signing key ready",
		"algorithm=" + ticket.Algorithm,
		"public_key=",
		"ticket_lifetime=",
	} {
		if !strings.Contains(logged, want) {
			t.Errorf("the startup log does not carry %q", want)
		}
	}

	// The key it printed is the key the pair on disk holds, rather than a placeholder
	// that happens to be 64 characters.
	keys, err := ticket.LoadOrCreate(opts.authDir)
	if err != nil {
		t.Fatalf("reading back the pair run created: %v", err)
	}
	if !strings.Contains(logged, keys.PublicHex()) {
		t.Error("the startup log does not carry the public key of the pair it kept")
	}
}

// **An unreadable key pair refuses before the port is bound**, in the order the store
// already does — and refusing at all is the point: regenerating over a damaged pair
// would invalidate every ticket in flight and every copy a game server had stored, and
// nobody would find out until a player was turned away.
//
// The listen address is one this test is already holding, so the ordering is what the
// assertion reads: reaching net.Listen would fail on the address instead.
func TestAnUnreadableSigningKeyRefusesBeforeThePortIsBound(t *testing.T) {
	t.Parallel()

	held, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listening: %v", err)
	}
	defer func() { _ = held.Close() }()

	opts := validOptions(t)
	opts.listen = held.Addr().String()

	// A first start, so there is a real pair to damage.
	if _, err := ticket.LoadOrCreate(opts.authDir); err != nil {
		t.Fatalf("ticket.LoadOrCreate: %v", err)
	}
	damaged := filepath.Join(opts.authDir, ticket.SigningKeyFileName)
	before, err := os.ReadFile(damaged)
	if err != nil {
		t.Fatalf("reading the signing key: %v", err)
	}
	if err := os.WriteFile(damaged, []byte("not a key"), 0o600); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	err = run(context.Background(), opts, discard())
	if err == nil {
		t.Fatal("run started with a signing key it cannot read")
	}
	if !strings.Contains(err.Error(), "opening the ticket signing key") {
		t.Errorf("run failed with %q, which is not the key pair refusing; the port was bound first", err)
	}
	// And nothing was written over: an operator can still restore the pair beside what
	// is there.
	after, err := os.ReadFile(damaged)
	if err != nil {
		t.Fatalf("reading the damaged key again: %v", err)
	}
	if string(after) != "not a key" {
		t.Error("the unreadable signing key was replaced with a fresh one")
	}
	if bytes.Equal(after, before) {
		t.Fatal("the test did not manage to damage the key it meant to")
	}
}

func TestNewLogger(t *testing.T) {
	t.Parallel()

	for _, level := range []string{"debug", "info", "warn", "error"} {
		for _, format := range []string{"text", "json", "JSON"} {
			if _, err := newLogger(level, format); err != nil {
				t.Errorf("newLogger(%q, %q): %v", level, format, err)
			}
		}
	}
	if _, err := newLogger("chatty", "text"); err == nil {
		t.Error("an unknown log level was accepted")
	}
	if _, err := newLogger("info", "yaml"); err == nil {
		t.Error("an unknown log format was accepted")
	}
}
