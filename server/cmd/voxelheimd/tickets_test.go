package main

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"crypto/tls"
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
	"github.com/FabioSM46/voxelheim-v2/server/internal/certs"
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
	return startTLS(t, mux)
}

// startTLS stands handler up behind TLS, holding a certificate of its own.
//
// **TLS, because there is no other way to reach an account service** (#131). A plaintext
// fake would test a hop this server refuses to make: `parseAccountService` requires https
// and `accountServiceClient` pins the certificate, so a fake standing up
// `httptest.NewServer` would be exercising nothing that exists.
//
// **And a certificate of its own, which `httptest.NewTLSServer` would not give it.** That
// helper presents one built-in certificate for every server it starts, so two of them are
// the same identity to a pin — and telling two services apart is the entire property under
// test. `certs.Ephemeral` is what the real services generate with.
func startTLS(t *testing.T, handler http.Handler) *httptest.Server {
	t.Helper()

	cert, err := certs.Ephemeral()
	if err != nil {
		t.Fatalf("certs.Ephemeral: %v", err)
	}
	srv := httptest.NewUnstartedServer(handler)
	srv.TLS = &tls.Config{Certificates: []tls.Certificate{cert}, MinVersion: tls.VersionTLS13}
	srv.StartTLS()
	t.Cleanup(srv.Close)
	return srv
}

// fingerprintOf is what -account-service-fingerprint has to carry for srv.
//
// Computed the same way `certs.Fingerprint` computes it — SHA-256 over the leaf's DER —
// rather than read from any helper this server ships, because the whole point of the
// number is that two programs arrive at it independently and get one string.
func fingerprintOf(t *testing.T, srv *httptest.Server) string {
	t.Helper()

	cert := srv.Certificate()
	if cert == nil {
		t.Fatal("the fake account service presents no certificate")
	}
	sum := sha256.Sum256(cert.Raw)
	return hex.EncodeToString(sum[:])
}

// clientFor is the pinned client this server would build for srv.
func clientFor(t *testing.T, srv *httptest.Server) *http.Client {
	t.Helper()

	client, err := accountServiceClient(options{accountServiceFingerprint: fingerprintOf(t, srv)})
	if err != nil {
		t.Fatalf("accountServiceClient: %v", err)
	}
	return client
}

// unmatchedFingerprint is a well-formed SHA-256 that no certificate has. Used where the
// flags have to parse and the connection is never expected to succeed.
func unmatchedFingerprint() string { return strings.Repeat("ab", sha256.Size) }

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
		worldName:                 testWorldName,
		accountService:            service.URL,
		accountServiceFingerprint: fingerprintOf(t, service),
	}, discard())
	if err != nil {
		t.Fatalf("openVerifier: %v", err)
	}

	// Gone. Not slow, not flaky: closed, with its listener released.
	service.Close()

	identities, err := session.NewIdentities(nil, nil, verifier, discard())
	if err != nil {
		t.Fatalf("NewIdentities: %v", err)
	}

	conn := newScriptedConn("offline")
	srv := newTestServer(t, newQueueTransport(conn), world.NewCache(testConfig().WorldSeed, 1, 64), nil)
	srv.identities = identities
	stop := start(t, srv)
	defer stop()

	if got := enterWorld(t, conn, helloFor(t, testAccount(6)), creationOf("Eivor")).PayloadType(); got != vnet.PayloadServerWelcome {
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
		"both key sources":      func(o *options) { o.accountService = "https://127.0.0.1:1" },
		"a key that is not hex": func(o *options) { o.ticketKey = "not a key" },
		"a key of the wrong length": func(o *options) {
			o.ticketKey = hex.EncodeToString(make([]byte, ed25519.PublicKeySize-1))
		},
		"no world name": func(o *options) { o.worldName = "" },
		"a world name this service would not mint for": func(o *options) { o.worldName = "Midgard" },
		"an account service with no scheme": func(o *options) {
			o.ticketKey, o.accountService = "", "127.0.0.1:8080"
			o.accountServiceFingerprint = unmatchedFingerprint()
		},
		"an account service with no host": func(o *options) {
			o.ticketKey, o.accountService = "", "https://"
			o.accountServiceFingerprint = unmatchedFingerprint()
		},
		"an account service carrying a query": func(o *options) {
			o.ticketKey, o.accountService = "", "https://127.0.0.1:8080/?key=1"
			o.accountServiceFingerprint = unmatchedFingerprint()
		},
		// **The four #131 added, and every one of them is a start that would otherwise
		// have been silently unauthenticated.** A plaintext address is the hop this
		// design no longer has; an account service with no fingerprint is a fetch with
		// nothing to check against, which is precisely the hole; and a fingerprint that
		// is not a SHA-256 is a check that could never match, so a start that accepted it
		// would fail later and blame the network.
		"an account service reached over plaintext": func(o *options) {
			o.ticketKey, o.accountService = "", "http://127.0.0.1:8080"
			o.accountServiceFingerprint = unmatchedFingerprint()
		},
		"an account service with no fingerprint to check": func(o *options) {
			o.ticketKey, o.accountService = "", "https://127.0.0.1:8080"
		},
		"a fingerprint that is not hex": func(o *options) {
			o.ticketKey, o.accountService = "", "https://127.0.0.1:8080"
			o.accountServiceFingerprint = "not a fingerprint"
		},
		"a fingerprint of the wrong length": func(o *options) {
			o.ticketKey, o.accountService = "", "https://127.0.0.1:8080"
			o.accountServiceFingerprint = hex.EncodeToString(make([]byte, sha256.Size-1))
		},
		// And the mirror of it: a fingerprint with nothing to check. Refused rather than
		// ignored, because an operator who wrote one there has a picture of this
		// deployment that is not the one they have.
		"a fingerprint beside a hand-copied key": func(o *options) {
			o.accountServiceFingerprint = unmatchedFingerprint()
		},
		"a fingerprint and nothing else": func(o *options) {
			o.ticketKey = ""
			o.accountServiceFingerprint = unmatchedFingerprint()
		},
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
		key, err := fetchTicketKey(context.Background(), clientFor(t, service), mustParseService(t, service.URL))
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

			key, err := fetchTicketKey(context.Background(), clientFor(t, service), mustParseService(t, service.URL))
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
	client := clientFor(t, service)
	service.Close()

	if _, err := fetchTicketKey(context.Background(), client, base); err == nil {
		t.Fatal("a key was read from an address nobody is answering on")
	}
}

// **The whole of #131, stated as the two outcomes it distinguishes.** A service
// presenting the pinned certificate answers; one presenting any other is refused before a
// byte of its answer is read.
//
// The second half is the one that matters, and it is the substitution the plaintext hop
// used to allow: the endpoint is unauthenticated on purpose, so anybody who could answer
// for that address handed this server their own public key — and this server would then
// admit every ticket they minted and refuse every real one, for as long as it ran,
// because the key is read once and kept.
//
// Both fingerprints are named in the refusal, because "the operator regenerated the
// service's certificate" and "somebody is answering for that address" are the same
// observation from here and nothing can tell them apart.
func TestReadingTheKeyRefusesAServiceThatIsNotTheOnePinned(t *testing.T) {
	t.Parallel()

	pinned := accountService(t, publishedKey(), http.StatusOK)
	if _, err := fetchTicketKey(context.Background(), clientFor(t, pinned), mustParseService(t, pinned.URL)); err != nil {
		t.Fatalf("the pinned account service was refused: %v", err)
	}

	// A second service, publishing exactly the same key. It differs in one thing only —
	// the certificate — which is the whole of what a pin can see, and it is what an
	// attacker able to answer for the address would differ in too.
	other := accountService(t, publishedKey(), http.StatusOK)
	client, err := accountServiceClient(options{accountServiceFingerprint: fingerprintOf(t, pinned)})
	if err != nil {
		t.Fatalf("accountServiceClient: %v", err)
	}

	_, err = fetchTicketKey(context.Background(), client, mustParseService(t, other.URL))
	if err == nil {
		t.Fatal("a key was read from a service presenting a certificate nobody pinned")
	}
	for what, want := range map[string]string{
		"the fingerprint that was expected":  fingerprintOf(t, pinned),
		"the fingerprint that was presented": fingerprintOf(t, other),
	} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("the refusal does not name %s", what)
		}
	}
}

// A fingerprint is read the same way whichever case it was written in, and refused when
// it is not a SHA-256 at all.
//
// Folded rather than refused, which is parseTicketKey's call and not internal/registry's:
// this value is decoded to bytes and compared as bytes, so a capital letter cannot
// silently mean a different certificate. A registry fingerprint is compared as text,
// where two spellings are two values that eventually fail to match.
func TestAFingerprintIsReadAsBytesAndNotAsText(t *testing.T) {
	t.Parallel()

	want := unmatchedFingerprint()
	for name, written := range map[string]string{
		"lowercase, as it is printed": want,
		"uppercase":                   strings.ToUpper(want),
		"with whitespace around it":   "  " + want + "\n",
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			raw, err := parseFingerprint(written)
			if err != nil {
				t.Fatalf("parseFingerprint: %v", err)
			}
			if hex.EncodeToString(raw) != want {
				t.Error("the fingerprint read back is not the one written down")
			}
		})
	}

	for name, written := range map[string]string{
		"not hex":        "not a fingerprint",
		"a short digest": hex.EncodeToString(make([]byte, sha256.Size-1)),
		"a long digest":  hex.EncodeToString(make([]byte, sha256.Size+1)),
		"nothing at all": "",
		"an ed25519 key": testPair.PublicHex()[:sha256.Size], // right alphabet, wrong length
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			if _, err := parseFingerprint(written); err == nil {
				t.Error("a value that is not a SHA-256 was accepted as a certificate fingerprint")
			}
		})
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

// Userinfo written into -account-service reaches neither a log line nor a refusal.
//
// An address is a flag value, and a flag value can carry userinfo: nothing about
// `http://ops:<secret>@accounts.example` is malformed, so nothing refuses it, and the
// credential is then inside a string this server writes down twice — once as the startup
// line's `ticket_key_source`, once inside every message naming the endpoint it failed to
// read. What this pins is that every spelling of the address this server writes goes
// through the loggable rendering.
//
// Both directions are here on purpose. The startup line is the one the review found, and
// a refusal is the path an operator is *more* likely to paste somewhere, because a server
// that came up cleanly gives nobody a reason to copy its log.
//
// **Two shapes of userinfo, because they fail differently.** `url.URL.Redacted` masks a
// password and keeps the username, so the password subtests below passed the moment #148
// landed while a deployment that puts a token in the username position with no password
// at all was still written down in full — there was nothing for the masking to mask. The
// password cases are kept exactly as they were and the token cases are added beside them.
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
			worldName:                 testWorldName,
			accountService:            withPassword(t, service.URL),
			accountServiceFingerprint: fingerprintOf(t, service),
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
			worldName:                 testWorldName,
			accountService:            withPassword(t, service.URL),
			accountServiceFingerprint: fingerprintOf(t, service),
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

	// The username half, and the reason this is a second case rather than a second
	// assertion on the first: some services take a token in that position with an empty
	// password, so there is no password for the standard library to mask and the whole
	// credential survives verbatim. Assembled through url.User rather than written into a
	// literal address, so the file never holds a string shaped like a credentialled URL.
	const token = "not-a-real-token"

	withToken := func(t *testing.T, raw string) string {
		t.Helper()

		parsed, err := url.Parse(raw)
		if err != nil {
			t.Fatalf("url.Parse(%q): %v", raw, err)
		}
		parsed.User = url.User(token)
		return parsed.String()
	}

	t.Run("a token in the username position, in the startup line", func(t *testing.T) {
		t.Parallel()

		service := accountService(t, publishedKey(), http.StatusOK)

		var logged strings.Builder
		log := slog.New(slog.NewTextHandler(&logged, &slog.HandlerOptions{Level: slog.LevelDebug}))
		if _, err := openVerifier(context.Background(), options{
			worldName:                 testWorldName,
			accountService:            withToken(t, service.URL),
			accountServiceFingerprint: fingerprintOf(t, service),
		}, log); err != nil {
			t.Fatalf("openVerifier: %v", err)
		}

		// The one line this start writes about the address: the startup line's
		// ticket_key_source, which is the loggable spelling of the endpoint.
		if strings.Contains(logged.String(), token) {
			t.Error("the token in -account-service's username was written to the startup log")
		}
		if !strings.Contains(logged.String(), ticketKeyPath) {
			t.Error("the startup log no longer names the endpoint the key was read from")
		}
	})

	// **Every way the read can fail, rather than one of them.** fetchTicketKey names the
	// endpoint in a message per refusal, and the refusal is the path that actually gets
	// copied: a server that came up cleanly gives nobody a reason to quote its log, while
	// one that will not start gets its error pasted into a ticket.
	t.Run("a token in the username position, in every refusal", func(t *testing.T) {
		t.Parallel()

		// Each case answers with the address to reach and the fingerprint to expect
		// there, because the two now travel together everywhere: a refusal reached with
		// no fingerprint would be a refusal about the flags rather than about the answer.
		for name, address := range map[string]func(t *testing.T) (string, string){
			"the service cannot be reached": func(t *testing.T) (string, string) {
				return withToken(t, "https://"+deadAddress(t)), unmatchedFingerprint()
			},
			"it presents a certificate nobody pinned": func(t *testing.T) (string, string) {
				service := accountService(t, publishedKey(), http.StatusOK)
				return withToken(t, service.URL), unmatchedFingerprint()
			},
			"it answers with a status that is not 200": func(t *testing.T) (string, string) {
				service := accountService(t, publishedKey(), http.StatusInternalServerError)
				return withToken(t, service.URL), fingerprintOf(t, service)
			},
			"its answer is longer than a key response can be": func(t *testing.T) (string, string) {
				service := accountService(t, strings.Repeat("x", maxTicketKeyResponseBytes+1), http.StatusOK)
				return withToken(t, service.URL), fingerprintOf(t, service)
			},
			"its answer is not the JSON this endpoint publishes": func(t *testing.T) (string, string) {
				service := accountService(t, "{not json", http.StatusOK)
				return withToken(t, service.URL), fingerprintOf(t, service)
			},
			"the key it publishes is for another algorithm": func(t *testing.T) (string, string) {
				body := fmt.Sprintf(`{"algorithm":"rsa","public_key":%q}`, testPair.PublicHex())
				service := accountService(t, body, http.StatusOK)
				return withToken(t, service.URL), fingerprintOf(t, service)
			},
			"the key it publishes is not hex": func(t *testing.T) (string, string) {
				body := fmt.Sprintf(`{"algorithm":%q,"public_key":"not a key"}`, ticket.Algorithm)
				service := accountService(t, body, http.StatusOK)
				return withToken(t, service.URL), fingerprintOf(t, service)
			},
		} {
			t.Run(name, func(t *testing.T) {
				t.Parallel()

				raw, fingerprint := address(t)
				_, err := openVerifier(context.Background(), options{
					worldName:                 testWorldName,
					accountService:            raw,
					accountServiceFingerprint: fingerprint,
				}, discard())
				if err == nil {
					t.Fatal("a key was read from a service that could not publish one")
				}
				// The error is never quoted into these messages. It is the one string
				// under test that holds the credential, and a test failure is a CI log.
				if strings.Contains(err.Error(), token) {
					t.Error("the token in -account-service's username is inside the refusal")
				}
				if !strings.Contains(err.Error(), ticketKeyPath) {
					t.Error("the refusal no longer names the endpoint it could not read")
				}
			})
		}
	})
}

// deadAddress is a host:port nothing is listening on.
//
// The operating system picks it — a listener is opened for its choice of a free port and
// closed again before the address is handed back — because a port written down here is a
// port some machine running these tests has something on.
func deadAddress(t *testing.T) string {
	t.Helper()

	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	addr := listener.Addr().String()
	if err := listener.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	return addr
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

	// A TLS listener rather than a plain one, and its certificate is what the fetch is
	// pinned to below: `accountServiceClient` is the only client this server has for an
	// account service, so a fake speaking plaintext would never get as far as the stall
	// this test is about. certs.Ephemeral is the same generator the real services use.
	cert, err := certs.Ephemeral()
	if err != nil {
		t.Fatalf("certs.Ephemeral: %v", err)
	}
	fingerprint, err := certs.Fingerprint(cert)
	if err != nil {
		t.Fatalf("certs.Fingerprint: %v", err)
	}

	plain, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	listener := tls.NewListener(plain, &tls.Config{
		Certificates: []tls.Certificate{cert},
		MinVersion:   tls.VersionTLS13,
	})
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

	base := mustParseService(t, "https://"+listener.Addr().String())
	client, err := accountServiceClient(options{accountServiceFingerprint: fingerprint})
	if err != nil {
		t.Fatalf("accountServiceClient: %v", err)
	}
	refused := make(chan error, 1)
	go func() {
		_, ferr := fetchTicketKey(context.Background(), client, base)
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
