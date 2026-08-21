package main

import (
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// accountService stands an account service up that publishes the package's own key,
// exactly as cmd/voxelheim-auth's endpoint does.
//
// A handwritten body rather than that command's own handler, deliberately: the two
// programs meet over HTTP, so what this test has to pin is that this server can read
// what that one *publishes* — field names and all — and a shared struct would hide a
// wire break behind a compile that still passes.
func accountService(t *testing.T, body string, status int) *httptest.Server {
	t.Helper()

	mux := http.NewServeMux()
	mux.HandleFunc(ticketKeyPath, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_, _ = w.Write([]byte(body))
	})
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	return srv
}

// publishedKey is what the account service answers for a working pair.
func publishedKey() string {
	return fmt.Sprintf(`{"algorithm":%q,"public_key":%q,"ticket_lifetime_seconds":%d}`,
		ticket.Algorithm, testPair.PublicHex(), int(ticket.Lifetime.Seconds()))
}

// **The headline property, and the only way to state it is to take the service away.**
//
// A game server verifies a signature instead of asking permission: it reads the account
// service's key once at startup and from then on admitting a player is arithmetic. This
// test reads the key over HTTP, shuts that service down, and *then* admits a session —
// so a build that had kept a connection, cached a lookup, or reached for the network on
// any part of the admission path could not pass it.
func TestAPlayerIsAdmittedWithTheAccountServiceGone(t *testing.T) {
	t.Parallel()

	service := accountService(t, publishedKey(), http.StatusOK)
	verifier, err := openVerifier(context.Background(), options{
		worldName:      testWorldName,
		accountService: service.URL,
	}, discard())
	if err != nil {
		t.Fatalf("openVerifier: %v", err)
	}

	// Gone. Not slow, not flaky: closed, with its listener released.
	service.Close()

	identities, err := session.NewIdentities(nil, verifier, discard())
	if err != nil {
		t.Fatalf("NewIdentities: %v", err)
	}

	conn := newScriptedConn("offline")
	srv := newTestServer(t, newQueueTransport(conn), world.NewCache(testConfig().WorldSeed, 1, 64), nil)
	srv.identities = identities
	stop := start(t, srv)
	defer stop()

	conn.in <- helloFor(t, testAccount(6))
	if got := firstReply(t, conn).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the session got %s with the account service down, want a welcome", got)
	}
}

// A server that cannot verify a ticket refuses to start, whichever way the key is
// missing.
//
// **The direction that matters is the permissive one.** Every case below could have
// been a warning and a server that comes up admitting whoever knocks, which is the
// second way in this design exists to remove — and it is the failure nobody notices,
// because a server with no doorman looks exactly like a server that is working.
func TestAServerWithNoWayToCheckATicketRefusesToStart(t *testing.T) {
	t.Parallel()

	valid := validOptions()
	cases := map[string]func(*options){
		"no key at all":         func(o *options) { o.ticketKey = "" },
		"both key sources":      func(o *options) { o.accountService = "http://127.0.0.1:1" },
		"a key that is not hex": func(o *options) { o.ticketKey = "not a key" },
		"a key of the wrong length": func(o *options) {
			o.ticketKey = hex.EncodeToString(make([]byte, ed25519.PublicKeySize-1))
		},
		"no world name": func(o *options) { o.worldName = "" },
		"a world name this service would not mint for": func(o *options) { o.worldName = "Midgard" },
		"an account service with no scheme":            func(o *options) { o.ticketKey, o.accountService = "", "127.0.0.1:8080" },
		"an account service with no host":              func(o *options) { o.ticketKey, o.accountService = "", "http://" },
		"an account service carrying a query":          func(o *options) { o.ticketKey, o.accountService = "", "http://127.0.0.1:8080/?key=1" },
	}

	for name, mutate := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			opts := valid
			mutate(&opts)
			if err := opts.validate(); err == nil {
				t.Error("the flags were accepted, so this server would start with no doorman")
			}
		})
	}

	// And the whole of run refuses before it takes a port or a directory, which is the
	// half validate cannot show: a server that validated and then started anyway would
	// pass every case above.
	broken := validOptions()
	broken.ticketKey = ""
	broken.worldDir = t.TempDir()
	if err := run(context.Background(), broken, discard()); err == nil {
		t.Error("run started a server that cannot verify a ticket")
	}
}

// The fetch reads what the account service publishes, and refuses everything else.
//
// Each case is a thing that can answer on that address and is not the key this server
// needs. **Refusing all of them is the same rule as refusing to start without a key**:
// the alternative to a key is not a lenient server, it is a server admitting whoever
// signed the tickets it was handed.
func TestReadingTheTicketKey(t *testing.T) {
	t.Parallel()

	t.Run("the published key is read", func(t *testing.T) {
		t.Parallel()

		service := accountService(t, publishedKey(), http.StatusOK)
		key, err := fetchTicketKey(context.Background(), mustParseService(t, service.URL), discard())
		if err != nil {
			t.Fatalf("fetchTicketKey: %v", err)
		}
		if !key.Equal(testPair.Public()) {
			t.Error("the key read back is not the one the service published")
		}
	})

	refusals := map[string]struct {
		body   string
		status int
	}{
		"another signature scheme": {
			body: fmt.Sprintf(`{"algorithm":"rsa","public_key":%q}`, testPair.PublicHex()),
		},
		"no algorithm at all": {
			body: fmt.Sprintf(`{"public_key":%q}`, testPair.PublicHex()),
		},
		"a key that is not hex": {
			body: fmt.Sprintf(`{"algorithm":%q,"public_key":"not a key"}`, ticket.Algorithm),
		},
		"a key of the wrong length": {
			body: fmt.Sprintf(`{"algorithm":%q,"public_key":%q}`, ticket.Algorithm,
				hex.EncodeToString(make([]byte, ed25519.PublicKeySize-1))),
		},
		"no key at all": {
			body: fmt.Sprintf(`{"algorithm":%q}`, ticket.Algorithm),
		},
		"something that is not JSON": {
			body: "<html>a proxy's error page</html>",
		},
		"an answer longer than a key response can be": {
			body: fmt.Sprintf(`{"algorithm":%q,"public_key":%q,"padding":%q}`, ticket.Algorithm,
				testPair.PublicHex(), strings.Repeat("x", maxTicketKeyResponseBytes)),
		},
		"a service that is not there": {
			body: "no such route", status: http.StatusNotFound,
		},
		"a service that is broken": {
			body: "the database is on fire", status: http.StatusInternalServerError,
		},
	}

	for name, tc := range refusals {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			status := tc.status
			if status == 0 {
				status = http.StatusOK
			}
			service := accountService(t, tc.body, status)

			key, err := fetchTicketKey(context.Background(), mustParseService(t, service.URL), discard())
			if err == nil {
				t.Fatal("the answer was accepted as this service's signing key")
			}
			if key != nil {
				t.Error("a key came back beside the error")
			}
			// Nothing of what answered is quoted into the error, because whatever
			// answered is not known to be the account service and its bytes are not
			// something to put in this server's log.
			if strings.Contains(err.Error(), tc.body) {
				t.Error("the refusal quotes the body it was answered with")
			}
		})
	}
}

// An address nobody is answering on is a refusal rather than a wait.
func TestReadingTheTicketKeyFromNobodyFails(t *testing.T) {
	t.Parallel()

	service := accountService(t, publishedKey(), http.StatusOK)
	base := mustParseService(t, service.URL)
	service.Close()

	if _, err := fetchTicketKey(context.Background(), base, discard()); err == nil {
		t.Fatal("a key was read from an address nobody is answering on")
	}
}

// The plaintext hop is a known gap, and an operator is told about it every time.
//
// #131 is where it is closed. What this pins is the honest half in the meantime: this
// server cannot tell that it reached the right account service, and the warning is what
// stops that being a silent property of the deployment.
func TestReadingTheKeyOverPlaintextWarns(t *testing.T) {
	t.Parallel()

	service := accountService(t, publishedKey(), http.StatusOK)

	var logged strings.Builder
	log := slog.New(slog.NewTextHandler(&logged, &slog.HandlerOptions{Level: slog.LevelWarn}))
	if _, err := fetchTicketKey(context.Background(), mustParseService(t, service.URL), log); err != nil {
		t.Fatalf("fetchTicketKey: %v", err)
	}

	if !strings.Contains(logged.String(), "unauthenticated") {
		t.Errorf("reading the key over http logged %q, which does not warn about the connection", logged.String())
	}
}

// The startup line names the key and the world, and never anything it should not.
//
// The public key is logged on purpose — it is public, and it is the one number an
// operator can compare against what the account service says — while the world id is a
// digest of a published name. Both are safe; a startup line that named neither would
// leave a fleet-wide key mismatch legible only as one refusal per player.
func TestTheStartupLineNamesTheKeyAndTheWorld(t *testing.T) {
	t.Parallel()

	var logged strings.Builder
	log := slog.New(slog.NewTextHandler(&logged, &slog.HandlerOptions{Level: slog.LevelInfo}))

	verifier, err := openVerifier(context.Background(), options{
		worldName: testWorldName,
		ticketKey: testTicketKey(),
	}, log)
	if err != nil {
		t.Fatalf("openVerifier: %v", err)
	}
	if verifier.World() != testWorld {
		t.Error("the verifier was built for a different world than the flag names")
	}

	for what, want := range map[string]string{
		"the world name": testWorldName,
		"the world id":   testWorld.String(),
		"the public key": testPair.PublicHex(),
		"the algorithm":  ticket.Algorithm,
	} {
		if !strings.Contains(logged.String(), want) {
			t.Errorf("the startup line does not name %s", what)
		}
	}
}

// A password written into -account-service reaches neither a log line nor a refusal.
//
// An address is a flag value, and a flag value can carry userinfo: nothing about
// `http://ops:<secret>@accounts.example` is malformed, so nothing refuses it, and the
// credential is then inside a string this server writes down twice — once as the startup
// line's `ticket_key_source`, once inside every message naming the endpoint it failed to
// read. `url.URL.Redacted` is the call the plaintext warning already made; what this pins
// is that every other spelling of the address goes through it too.
//
// Both directions are here on purpose. The startup line is the one the review found, and
// a refusal is the path an operator is *more* likely to paste somewhere, because a server
// that came up cleanly gives nobody a reason to copy its log.
func TestAPasswordInTheAccountServiceAddressIsNeverWrittenDown(t *testing.T) {
	t.Parallel()

	const password = "not-a-real-password"

	withPassword := func(t *testing.T, raw string) string {
		t.Helper()

		parsed, err := url.Parse(raw)
		if err != nil {
			t.Fatalf("url.Parse(%q): %v", raw, err)
		}
		parsed.User = url.UserPassword("ops", password)
		return parsed.String()
	}

	t.Run("the startup line", func(t *testing.T) {
		t.Parallel()

		service := accountService(t, publishedKey(), http.StatusOK)

		var logged strings.Builder
		log := slog.New(slog.NewTextHandler(&logged, &slog.HandlerOptions{Level: slog.LevelDebug}))
		if _, err := openVerifier(context.Background(), options{
			worldName:      testWorldName,
			accountService: withPassword(t, service.URL),
		}, log); err != nil {
			t.Fatalf("openVerifier: %v", err)
		}

		if strings.Contains(logged.String(), password) {
			t.Error("the password from -account-service was written to the startup log")
		}
		if !strings.Contains(logged.String(), ticketKeyPath) {
			t.Error("the startup log no longer names the endpoint the key was read from")
		}
	})

	t.Run("a refusal", func(t *testing.T) {
		t.Parallel()

		service := accountService(t, publishedKey(), http.StatusInternalServerError)

		_, err := openVerifier(context.Background(), options{
			worldName:      testWorldName,
			accountService: withPassword(t, service.URL),
		}, discard())
		if err == nil {
			t.Fatal("a key was read from a service that answered 500")
		}
		if strings.Contains(err.Error(), password) {
			t.Error("the password from -account-service is inside the refusal")
		}
		if !strings.Contains(err.Error(), ticketKeyPath) {
			t.Error("the refusal no longer names the endpoint it could not read")
		}
	})
}

// A key copied by hand is read the same way whichever case it was written in, because
// it is decoded to bytes rather than compared as text.
//
// The opposite call from `internal/registry`, which refuses an uppercase certificate
// fingerprint rather than folding it, and the difference is what happens to the string:
// a fingerprint is compared as text, so two spellings are two values that eventually
// fail to match.
func TestATicketKeyIsReadAsBytesAndNotAsText(t *testing.T) {
	t.Parallel()

	for name, written := range map[string]string{
		"lowercase, as published":   testPair.PublicHex(),
		"uppercase":                 strings.ToUpper(testPair.PublicHex()),
		"with whitespace around it": "  " + testPair.PublicHex() + "\n",
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			key, err := parseTicketKey(written)
			if err != nil {
				t.Fatalf("parseTicketKey: %v", err)
			}
			if !key.Equal(testPair.Public()) {
				t.Error("the key read back is not the one written down")
			}
		})
	}

	if _, err := parseTicketKey("not a key"); err == nil {
		t.Error("a key that is not hex was accepted")
	}
	short := hex.EncodeToString(make([]byte, ed25519.PublicKeySize-1))
	if _, err := parseTicketKey(short); !errors.Is(err, ticket.ErrPublicKeySize) {
		t.Errorf("a short key was refused with %v, want ErrPublicKeySize", err)
	}
}

// mustParseService is parseAccountService for a URL a test built and knows is good.
func mustParseService(t *testing.T, raw string) *url.URL {
	t.Helper()

	base, err := parseAccountService(raw)
	if err != nil {
		t.Fatalf("parseAccountService(%q): %v", raw, err)
	}
	return base
}

// A guard on the clock the fetch is bounded by rather than on a number: the request
// budget has to be something an operator watching a server come up would wait through,
// and something a wrong address ends rather than hangs on.
func TestTheKeyFetchIsBounded(t *testing.T) {
	t.Parallel()

	if fetchTicketKeyTimeout <= 0 {
		t.Error("the fetch has no timeout, so a silent address is a server that never starts")
	}
	if fetchTicketKeyTimeout > time.Minute {
		t.Errorf("the fetch waits up to %s, which is longer than anybody watches a start", fetchTicketKeyTimeout)
	}
}

// A service that advertises a body and then goes quiet costs the start nothing.
//
// The deadline on the request bounds a body read, so this was never the hang it looks
// like — but a response left unread on the way out is still a response somebody has to
// stop waiting for, and draining one for tidiness meant sitting on that deadline to copy
// nothing to io.Discard. The response is closed rather than drained, so the refusal an
// operator gets is the one the status line already justified, at the moment it arrived
// rather than a fetch budget later.
func TestAStallingAccountServiceDoesNotCostTheStartBudget(t *testing.T) {
	t.Parallel()

	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	t.Cleanup(func() { _ = listener.Close() })

	// Closed before the listener is, because cleanups run in reverse: the handler below
	// is parked on it, and a handler still parked is a connection that will not close.
	release := make(chan struct{})
	t.Cleanup(func() { close(release) })

	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go func(c net.Conn) {
				defer func() { _ = c.Close() }()
				_, _ = c.Read(make([]byte, 4096))
				// A refusal, a length far past anything a key response may be, and then
				// nothing whatsoever until this test is over.
				_, _ = io.WriteString(c, "HTTP/1.1 500 Internal Server Error\r\n"+
					"Content-Type: application/json\r\nContent-Length: 1000000\r\n\r\n")
				<-release
			}(conn)
		}
	}()

	// Generous beside the microseconds this takes and far under fetchTicketKeyTimeout,
	// so what a failure here means is that the drain came back, not that the machine is
	// slow.
	const patience = 2 * time.Second

	base := mustParseService(t, "http://"+listener.Addr().String())
	refused := make(chan error, 1)
	go func() {
		_, ferr := fetchTicketKey(context.Background(), base, discard())
		refused <- ferr
	}()

	select {
	case ferr := <-refused:
		if ferr == nil {
			t.Fatal("a key was read from a service that answered 500 and then sent no body")
		}
	case <-time.After(patience):
		t.Fatalf("the fetch was still waiting on a silent body after %s, which is the start "+
			"budget spent on a response nobody reads", patience)
	}
}
