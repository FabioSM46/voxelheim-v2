package main

import (
	"bytes"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/registry"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// The fixtures. **Every address and every digest here is invented**, because this repository
// is public and this endpoint is about the addresses of people's houses: the addresses are
// TEST-NET-1 (192.0.2.0/24, reserved for documentation by RFC 5737) and the reserved
// `example.invalid` domain, and the fingerprints are patterns no certificate produces.
const (
	fixtureKey = "a-registration-key-long-enough-to-be-accepted"

	fixtureFingerprint = "abababababababababababababababababababababababababababababababab"
	movedFingerprint   = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"

	fixtureAddress = "192.0.2.10:7777"
	movedAddress   = "192.0.2.200:7777"
)

// registryService is a service with a real registry in a directory of its own, a real signing
// pair, and a registration key.
//
// Real rather than doubles throughout: what these tests are about is what ends up on disk and
// what a ticket a game server would accept can do, and a double would be asserting against
// this test's own idea of those.
func registryService(t *testing.T, log *slog.Logger) *service {
	t.Helper()

	return registryServiceWithKey(t, log, fixtureKey)
}

// registryServiceWithKey is registryService over a chosen key, or over none: an empty raw key
// leaves registration unconfigured, which is the state a deployment is in until an operator
// has invented one.
func registryServiceWithKey(t *testing.T, log *slog.Logger, raw string) *service {
	t.Helper()

	authDir := filepath.Join(t.TempDir(), "auth")
	keys, err := ticket.LoadOrCreate(authDir)
	if err != nil {
		t.Fatalf("ticket.LoadOrCreate: %v", err)
	}
	servers, err := registry.OpenStore(authDir)
	if err != nil {
		t.Fatalf("registry.OpenStore: %v", err)
	}

	svc := &service{log: log, keys: keys, servers: servers}
	if raw != "" {
		key, err := registry.ParseKey(raw)
		if err != nil {
			t.Fatalf("registry.ParseKey: %v", err)
		}
		svc.registrationKey = &key
	}
	return svc
}

// callWith drives one request through the real route table with an Authorization header,
// which is the only way these tests reach a handler: a handler called directly is a handler
// tested without the method and the pattern that CI's route-table test pins.
//
// An empty credential sends no header at all, which is the "nothing was presented" case.
func callWith(t *testing.T, svc *service, method, path, credential, body string) *httptest.ResponseRecorder {
	t.Helper()

	var req *http.Request
	if body == "" {
		req = httptest.NewRequest(method, path, nil)
	} else {
		req = httptest.NewRequest(method, path, strings.NewReader(body))
	}
	if credential != "" {
		req.Header.Set("Authorization", credential)
	}
	rec := httptest.NewRecorder()
	newMux(svc.routes()).ServeHTTP(rec, req)
	return rec
}

// aRegistration is a body every field of which is accepted. The cases below mutate the one
// field under test rather than building a literal each time: a literal that omits a field is
// a case that passes for a reason it did not mean.
func aRegistration() map[string]string {
	return map[string]string{
		"name":               "midgard",
		"display_name":       "Midgard",
		"address":            fixtureAddress,
		"certificate_sha256": fixtureFingerprint,
	}
}

func registerBody(t *testing.T, fields map[string]string) string {
	t.Helper()

	body, err := json.Marshal(fields)
	if err != nil {
		t.Fatalf("building a registration: %v", err)
	}
	return string(body)
}

// registerWith posts a registration presenting a key.
func registerWith(t *testing.T, svc *service, key string, fields map[string]string) *httptest.ResponseRecorder {
	t.Helper()

	credential := ""
	if key != "" {
		credential = "Bearer " + key
	}
	return callWith(t, svc, http.MethodPost, "/v1/servers", credential, registerBody(t, fields))
}

// registerOK posts a registration with the fixture key and fails the test if it was refused.
func registerOK(t *testing.T, svc *service, fields map[string]string) registerResponse {
	t.Helper()

	rec := registerWith(t, svc, fixtureKey, fields)
	if rec.Code != http.StatusOK {
		t.Fatalf("POST /v1/servers answered %d: %s", rec.Code, rec.Body.String())
	}
	var answer registerResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &answer); err != nil {
		t.Fatalf("reading the registration response: %v", err)
	}
	return answer
}

// anAccountTicket is a real account ticket signed by this service's own pair — the credential
// a player holds after signing in without naming a world.
func anAccountTicket(t *testing.T, svc *service) string {
	t.Helper()

	minted, _, err := svc.keys.MintAccountTicket(anAccountID(), time.Now())
	if err != nil {
		t.Fatalf("MintAccountTicket: %v", err)
	}
	return minted.Encode()
}

func anAccountID() ticket.AccountID {
	var id ticket.AccountID
	for i := range id {
		id[i] = byte(i + 1)
	}
	return id
}

// listWith reads the list presenting a credential.
func listWith(t *testing.T, svc *service, credential string) *httptest.ResponseRecorder {
	t.Helper()

	if credential != "" {
		credential = "Bearer " + credential
	}
	return callWith(t, svc, http.MethodGet, "/v1/servers", credential, "")
}

// listOK reads the list with a fresh account ticket and fails the test if it was refused.
func listOK(t *testing.T, svc *service) serverListResponse {
	t.Helper()

	rec := listWith(t, svc, anAccountTicket(t, svc))
	if rec.Code != http.StatusOK {
		t.Fatalf("GET /v1/servers answered %d: %s", rec.Code, rec.Body.String())
	}
	var answer serverListResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &answer); err != nil {
		t.Fatalf("reading the list: %v", err)
	}
	return answer
}

// **Registration is authenticated, and this is the criterion the security of the whole list
// rests on.** Anybody able to register could otherwise put their own address in the list under
// a name players trust — and a client that trusts the list would connect to it and accept the
// certificate it was told to expect, which is a worse outcome than the trust-on-first-use this
// list replaces.
func TestARegistrationWithoutTheOperatorsKeyIsRefused(t *testing.T) {
	t.Parallel()

	for name, credential := range map[string]string{
		"no credential at all":              "",
		"the wrong key":                     "Bearer another-registration-key-long-enough",
		"an empty bearer value":             "Bearer ",
		"another scheme":                    "Basic " + base64.StdEncoding.EncodeToString([]byte("a:"+fixtureKey)),
		"the key with no scheme":            fixtureKey,
		"the key with whitespace around it": "Bearer  " + fixtureKey + " ",
		// Reachable by anybody — the credential has to be read to be refused — so what
		// this case pins is that an oversized one is still one refusal and not an
		// invitation to hand this process a header to hash. registry.Key.Matches turns it
		// away on length; see registry.MaxKeyBytes for why that cannot turn away a real key.
		"a credential longer than any key": "Bearer " + strings.Repeat("k", registry.MaxKeyBytes+1),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			svc := registryService(t, discard())
			rec := callWith(t, svc, http.MethodPost, "/v1/servers", credential, registerBody(t, aRegistration()))

			if rec.Code != http.StatusUnauthorized {
				t.Errorf("answered %d, want 401", rec.Code)
			}
			// **A 401 means "come back with a credential", and the header is what makes it
			// mean that.** signin.go declines to use this status precisely because it has
			// no scheme to name; these two routes do.
			if got := rec.Header().Get("WWW-Authenticate"); got != "Bearer" {
				t.Errorf("the refusal carries WWW-Authenticate %q, want Bearer", got)
			}
			// One answer for every way of getting it wrong, so nobody guessing learns
			// which guesses are getting warmer.
			if got := refusalCode(t, rec); got != errUnauthorized {
				t.Errorf("answered %q, want %q", got, errUnauthorized)
			}
			// And nothing was written, which is asserted rather than assumed.
			if got := listOK(t, svc); len(got.Servers) != 0 {
				t.Errorf("a refused registration put %d servers in the list", len(got.Servers))
			}
		})
	}
}

// A deployment nobody has given a key to refuses every registration, and says what is missing
// rather than being silently absent. It is not a 401: no credential would work.
func TestRegistrationRefusesEverythingUntilAKeyIsConfigured(t *testing.T) {
	t.Parallel()

	svc := registryServiceWithKey(t, discard(), "")

	for name, credential := range map[string]string{
		"no credential":   "",
		"a plausible key": "Bearer " + fixtureKey,
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			rec := callWith(t, svc, http.MethodPost, "/v1/servers", credential, registerBody(t, aRegistration()))
			if rec.Code != http.StatusServiceUnavailable {
				t.Errorf("answered %d, want 503", rec.Code)
			}
			if got := refusalCode(t, rec); got != errRegistrationNotConfigured {
				t.Errorf("answered %q, want %q", got, errRegistrationNotConfigured)
			}
		})
	}

	// The list still works: it is read with a ticket, not with the registration key. An
	// unconfigured registration is an empty list rather than a broken service.
	if got := listOK(t, svc); got.Servers == nil || len(got.Servers) != 0 {
		t.Errorf("the list answered %+v, want an empty list", got.Servers)
	}
}

// The whole path: a server registers, and the list answers with the four things a client needs
// to reach it and to know it is the right one.
func TestARegisteredServerIsInTheListWithItsFingerprint(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())

	answer := registerOK(t, svc, aRegistration())
	if !answer.Created {
		t.Error("a first registration did not report itself as new")
	}
	if answer.OfflineAfterSeconds != int(registry.OfflineAfter.Seconds()) {
		t.Errorf("the registration published a window of %ds, want %d",
			answer.OfflineAfterSeconds, int(registry.OfflineAfter.Seconds()))
	}

	got := listOK(t, svc)
	if len(got.Servers) != 1 {
		t.Fatalf("the list holds %d servers, want 1", len(got.Servers))
	}
	entry := got.Servers[0]
	if entry.Name != "midgard" || entry.DisplayName != "Midgard" || entry.Address != fixtureAddress {
		t.Errorf("the list answered %+v, want the registration's own fields", entry)
	}
	// **The number that ends the trust chain.** The client verifies the certificate against
	// this rather than against a file it wrote the first time it connected.
	if entry.CertificateSHA256 != fixtureFingerprint {
		t.Errorf("the list carries fingerprint %q, want the one that was registered", entry.CertificateSHA256)
	}
	if !entry.Online {
		t.Error("a server that registered a moment ago is not online")
	}
	if entry.LastSeen.IsZero() {
		t.Error("the list says nothing about when the server was last heard from")
	}
}

// **The criterion a changing home address turns on.** A re-registration from the same server
// updates its address, so the list serves the one it last announced and nobody has to be told
// anything.
func TestAReregistrationMovesTheAddressTheListServes(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())
	registerOK(t, svc, aRegistration())

	// The same server after its connection got a new address overnight — and, because a
	// restart without a kept key does this, presenting a different certificate.
	moved := aRegistration()
	moved["address"] = movedAddress
	moved["certificate_sha256"] = movedFingerprint

	if answer := registerOK(t, svc, moved); answer.Created {
		t.Error("a re-registration of a known name reported itself as new")
	}

	got := listOK(t, svc)
	if len(got.Servers) != 1 {
		t.Fatalf("the list holds %d servers after a re-registration, want 1", len(got.Servers))
	}
	if got.Servers[0].Address != movedAddress {
		t.Errorf("the list serves %q, want the address last announced", got.Servers[0].Address)
	}
	if got.Servers[0].CertificateSHA256 != movedFingerprint {
		t.Errorf("the list serves fingerprint %q, want the one last announced", got.Servers[0].CertificateSHA256)
	}
}

// **A malformed fingerprint is refused**, with its own code so the operator is sent to the
// line of their configuration that is wrong rather than to their JSON encoder.
func TestAMalformedFingerprintIsRefused(t *testing.T) {
	t.Parallel()

	for name, fingerprint := range map[string]string{
		"absent":          "",
		"too short":       fixtureFingerprint[:len(fixtureFingerprint)-1],
		"too long":        fixtureFingerprint + "a",
		"in capitals":     strings.ToUpper(fixtureFingerprint),
		"not hex":         strings.Repeat("z", len(fixtureFingerprint)),
		"colon-separated": "ab:cd:" + fixtureFingerprint[6:],
		"a base64 digest": base64.RawStdEncoding.EncodeToString(make([]byte, 32)),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			svc := registryService(t, discard())
			fields := aRegistration()
			fields["certificate_sha256"] = fingerprint

			rec := registerWith(t, svc, fixtureKey, fields)
			if rec.Code != http.StatusBadRequest {
				t.Errorf("answered %d, want 400", rec.Code)
			}
			if got := refusalCode(t, rec); got != errFingerprintNotADigest {
				t.Errorf("answered %q, want %q", got, errFingerprintNotADigest)
			}
			if got := listOK(t, svc); len(got.Servers) != 0 {
				t.Errorf("a refused registration put %d servers in the list", len(got.Servers))
			}
		})
	}
}

// The other three fields, each with the code that names it. **Split by field on purpose**,
// which is the opposite of the sign-in routes' rule and the opposite because the callers are
// opposites: this one holds the operator's own key and has one configuration to fix.
func TestEachRefusedFieldIsNamedInTheRefusal(t *testing.T) {
	t.Parallel()

	cases := map[string]struct {
		field, value, want string
	}{
		"no name":                       {"name", "", errServerNotNamed},
		"a name with a capital":         {"name", "Midgard", errServerNotNamed},
		"a name with a slash":           {"name", "../../etc", errServerNotNamed},
		"a name too long":               {"name", strings.Repeat("a", registry.MaxNameBytes+1), errServerNotNamed},
		"a display name too long":       {"display_name", strings.Repeat("a", registry.MaxDisplayNameBytes+1), errDisplayNameRefused},
		"a display name with a newline": {"display_name", "Mid\ngard", errDisplayNameRefused},
		"no address":                    {"address", "", errAddressRefused},
		"an address with no port":       {"address", "192.0.2.10", errAddressRefused},
		"an address with no host":       {"address", ":7777", errAddressRefused},
		"a named port":                  {"address", "192.0.2.10:voxelheim", errAddressRefused},
		"a port past a uint16":          {"address", "192.0.2.10:99999", errAddressRefused},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			svc := registryService(t, discard())
			fields := aRegistration()
			fields[tc.field] = tc.value

			rec := registerWith(t, svc, fixtureKey, fields)
			if rec.Code != http.StatusBadRequest {
				t.Errorf("answered %d, want 400", rec.Code)
			}
			if got := refusalCode(t, rec); got != tc.want {
				t.Errorf("answered %q, want %q", got, tc.want)
			}
			if got := listOK(t, svc); len(got.Servers) != 0 {
				t.Errorf("a refused registration put %d servers in the list", len(got.Servers))
			}
		})
	}
}

// A server name is the world name, and this is what that buys: a name the registry accepts is
// always a name `POST /v1/signin/discord/finish` will mint a ticket for. A registry that
// accepted a name the ticket service would not is a server a player can see and cannot join.
func TestARegisteredNameIsAlwaysANameATicketCanBeMintedFor(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())
	registerOK(t, svc, aRegistration())

	for _, entry := range listOK(t, svc).Servers {
		if _, err := ticket.WorldIDFor(entry.Name); err != nil {
			t.Errorf("the list carries %q, which no ticket can be minted for: %v", entry.Name, err)
		}
	}
}

// An absent display name becomes the name, so an announcer has one fewer value to configure
// and the list never has a blank column. The default lives in the handler because "the field
// was absent" is a fact about a request, and the store never sees one.
func TestAnAbsentDisplayNameBecomesTheName(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())
	fields := aRegistration()
	delete(fields, "display_name")
	registerOK(t, svc, fields)

	got := listOK(t, svc)
	if len(got.Servers) != 1 || got.Servers[0].DisplayName != "midgard" {
		t.Errorf("the list answered %+v, want the display name defaulted to the server name", got.Servers)
	}
}

// A body that is not JSON, and one too large to be a registration. Both are 400s that name
// the body rather than a field, because no field was read.
func TestAnUnreadableRegistrationBodyIsRefused(t *testing.T) {
	t.Parallel()

	for name, body := range map[string]string{
		"not JSON at all":       `{"name":`,
		"a JSON array":          `["midgard"]`,
		"a body past the bound": `{"name":"midgard","display_name":"` + strings.Repeat("a", maxRegistrationRequestBytes) + `"}`,
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			svc := registryService(t, discard())
			rec := callWith(t, svc, http.MethodPost, "/v1/servers", "Bearer "+fixtureKey, body)
			if rec.Code != http.StatusBadRequest {
				t.Errorf("answered %d, want 400", rec.Code)
			}
			if got := refusalCode(t, rec); got != errMalformedRequest {
				t.Errorf("answered %q, want %q", got, errMalformedRequest)
			}
		})
	}
}

// **The list is readable only by an authenticated account**, which is what keeps it from being
// a public directory of somebody's home address.
func TestTheListIsRefusedToAnUnauthenticatedReader(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())
	registerOK(t, svc, aRegistration())

	// A ticket signed by a service that is not this one, which is what an invented ticket
	// looks like from here.
	other := registryService(t, discard())

	for name, credential := range map[string]string{
		"no credential at all":           "",
		"something that is not a ticket": "Bearer not-a-ticket",
		"a ticket of the wrong length":   "Bearer " + base64.RawURLEncoding.EncodeToString(make([]byte, ticket.Size-1)),
		"another service's ticket":       "Bearer " + anAccountTicket(t, other),
		"the registration key":           "Bearer " + fixtureKey,
		// Valid base64url of a length no ticket has. ticket.Decode refuses it on that
		// length before decoding any of it, which is the same argument one endpoint over.
		"a credential longer than any ticket": "Bearer " + strings.Repeat("A", ticket.EncodedSize+1),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			rec := callWith(t, svc, http.MethodGet, "/v1/servers", credential, "")
			if rec.Code != http.StatusUnauthorized {
				t.Errorf("answered %d, want 401", rec.Code)
			}
			if got := rec.Header().Get("WWW-Authenticate"); got != "Bearer" {
				t.Errorf("the refusal carries WWW-Authenticate %q, want Bearer", got)
			}
			if got := refusalCode(t, rec); got != errUnauthorized {
				t.Errorf("answered %q, want %q", got, errUnauthorized)
			}
			// **The refusal carries no part of the list.** A directory that leaks through
			// its own error messages is not behind a credential at all.
			if strings.Contains(rec.Body.String(), fixtureAddress) ||
				strings.Contains(rec.Body.String(), fixtureFingerprint) {
				t.Error("a refused list read answered with something from the list")
			}
		})
	}
}

// **Either kind of ticket reads the list**, which is the contract this issue turns on: an
// account ticket, because a player cannot name a world before they have seen the list, and a
// world-scoped one, because somebody already holding one should not have to sign in again.
func TestTheListTakesAnAccountTicketOrAWorldTicket(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())
	registerOK(t, svc, aRegistration())

	world, err := ticket.WorldIDFor("midgard")
	if err != nil {
		t.Fatalf("WorldIDFor: %v", err)
	}
	worldTicket, _, err := svc.keys.Mint(anAccountID(), world, time.Now())
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}

	for name, credential := range map[string]string{
		"an account ticket": anAccountTicket(t, svc),
		"a world ticket":    worldTicket.Encode(),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			rec := listWith(t, svc, credential)
			if rec.Code != http.StatusOK {
				t.Fatalf("answered %d: %s", rec.Code, rec.Body.String())
			}
			var answer serverListResponse
			if err := json.Unmarshal(rec.Body.Bytes(), &answer); err != nil {
				t.Fatalf("reading the list: %v", err)
			}
			if len(answer.Servers) != 1 {
				t.Errorf("the list holds %d servers, want 1", len(answer.Servers))
			}
		})
	}
}

// An expired ticket is the one refusal split out of errUnauthorized, because it is the
// difference between a client sending its player back to the login screen with a line saying
// why and one showing them a failure to interpret. There is no revocation, so this is the only
// way a ticket ever stops working.
func TestAnExpiredTicketIsRefusedAndSaysSo(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())
	registerOK(t, svc, aRegistration())

	// Minted far enough in the past that it has already run out. `now` is a parameter to
	// Mint for exactly this: nothing waits eight hours.
	expired, _, err := svc.keys.MintAccountTicket(anAccountID(), time.Now().Add(-ticket.Lifetime-time.Hour))
	if err != nil {
		t.Fatalf("MintAccountTicket: %v", err)
	}

	rec := listWith(t, svc, expired.Encode())
	if rec.Code != http.StatusUnauthorized {
		t.Errorf("answered %d, want 401", rec.Code)
	}
	if got := refusalCode(t, rec); got != errTicketExpired {
		t.Errorf("answered %q, want %q", got, errTicketExpired)
	}
}

// **Shown as offline, never dropped.** A server that has stopped announcing is still in the
// list with the address it last gave; what changes is one boolean a player reads as "you
// probably cannot join this right now".
func TestAServerAbsentTooLongIsShownOfflineRatherThanDropped(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())

	// Put a server in that went quiet an hour ago, through the store, because the endpoint
	// deliberately does not let an announcer choose when it was last heard from.
	quiet := registry.Server{
		Name:        "asgard",
		DisplayName: "Asgard",
		Address:     "asgard.example.invalid:7777",
		Fingerprint: movedFingerprint,
		LastSeen:    time.Now().Add(-registry.OfflineAfter - time.Hour),
	}
	if _, err := svc.servers.Register(quiet); err != nil {
		t.Fatalf("Register: %v", err)
	}
	registerOK(t, svc, aRegistration())

	got := listOK(t, svc)
	if len(got.Servers) != 2 {
		t.Fatalf("the list holds %d servers, want 2 — an absent server was dropped", len(got.Servers))
	}

	byName := map[string]serverListEntry{}
	for _, entry := range got.Servers {
		byName[entry.Name] = entry
	}
	if byName["asgard"].Online {
		t.Error("a server unheard from for an hour is reported online")
	}
	if byName["asgard"].Address != quiet.Address {
		t.Error("an offline server lost the address it last announced")
	}
	if byName["asgard"].CertificateSHA256 != quiet.Fingerprint {
		t.Error("an offline server lost the fingerprint it last announced")
	}
	if !byName["midgard"].Online {
		t.Error("a server that announced a moment ago is not online")
	}
	if got.OfflineAfterSeconds != int(registry.OfflineAfter.Seconds()) {
		t.Errorf("the list published a window of %ds, want %d",
			got.OfflineAfterSeconds, int(registry.OfflineAfter.Seconds()))
	}
}

// An empty registry answers `[]` rather than `null`: a client decoding this always has a list
// to iterate, and a second shape for "no servers" is a case every consumer has to handle.
func TestAnEmptyListIsAnEmptyArray(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())
	rec := listWith(t, svc, anAccountTicket(t, svc))
	if rec.Code != http.StatusOK {
		t.Fatalf("answered %d: %s", rec.Code, rec.Body.String())
	}
	if !strings.Contains(rec.Body.String(), `"servers":[]`) {
		t.Errorf("an empty list answered %s, want an empty array", strings.TrimSpace(rec.Body.String()))
	}
}

// The list is ordered by name, so it does not reshuffle between two reads.
func TestTheListIsOrderedByName(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())
	for _, name := range []string{"vanaheim", "asgard", "midgard"} {
		fields := aRegistration()
		fields["name"] = name
		registerOK(t, svc, fields)
	}

	var got []string
	for _, entry := range listOK(t, svc).Servers {
		got = append(got, entry.Name)
	}
	if want := "asgard,midgard,vanaheim"; strings.Join(got, ",") != want {
		t.Errorf("the list is %v, want %s", got, want)
	}
}

// The method is part of the route, so a wrong one is a 405 from the mux rather than the first
// four lines of every handler — and, here, rather than a registration that answered a GET.
func TestTheServerRoutesAnswerOnlyTheirOwnMethods(t *testing.T) {
	t.Parallel()

	svc := registryService(t, discard())

	for method, want := range map[string]int{
		http.MethodPut:    http.StatusMethodNotAllowed,
		http.MethodDelete: http.StatusMethodNotAllowed,
		http.MethodPatch:  http.StatusMethodNotAllowed,
	} {
		rec := callWith(t, svc, method, "/v1/servers", "Bearer "+fixtureKey, "")
		if rec.Code != want {
			t.Errorf("%s /v1/servers answered %d, want %d", method, rec.Code, want)
		}
	}
}

// **Neither the registration key nor a registered address reaches the log.**
//
// The key is a credential: whoever holds it can put their own address in the list under a name
// players trust. The address is somebody's house, and it is the reason the list is behind a
// credential at all — a value that must not be published is a value that must not be in a log
// line either, and a log line outlives the process that wrote it.
//
// Both handlers, because the JSON one is the one a Stringer would not have saved. A whole
// registration plus every refusal a handler can log, so that the paths where a value is most
// likely to be pasted into a message are all exercised in one capture. Every value is looked
// for in hex, base64, base64url and raw, because a redaction that only covers the shape the
// happy path uses is not a redaction.
func TestNeitherTheRegistrationKeyNorAnAddressReachesTheLog(t *testing.T) {
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
			svc := registryService(t, slog.New(build(&out)))

			// A registration that works, a re-registration, and every refusal.
			registerOK(t, svc, aRegistration())
			moved := aRegistration()
			moved["address"] = movedAddress
			registerOK(t, svc, moved)

			registerWith(t, svc, "", aRegistration())                                          // no key
			registerWith(t, svc, "a-wrong-key-that-is-long-enough-to-be-one", aRegistration()) // a wrong key
			callWith(t, svc, http.MethodPost, "/v1/servers", "Bearer "+fixtureKey, `{"name":`) // an unreadable body

			bad := aRegistration()
			bad["certificate_sha256"] = "not a digest"
			registerWith(t, svc, fixtureKey, bad)

			badAddr := aRegistration()
			badAddr["address"] = "192.0.2.10:voxelheim"
			registerWith(t, svc, fixtureKey, badAddr)

			// The list, refused and served, at debug so the read line is captured too.
			listWith(t, svc, "not-a-ticket")
			listOK(t, svc)

			logged := out.String()
			if logged == "" {
				t.Fatal("nothing was logged, so this test proves nothing")
			}

			secrets := map[string][]byte{
				"the registration key": []byte(fixtureKey),
				"a registered address": []byte(fixtureAddress),
				"a moved address":      []byte(movedAddress),
			}
			for what, value := range secrets {
				for encoding, rendered := range map[string]string{
					"raw":       string(value),
					"hex":       hex.EncodeToString(value),
					"base64":    base64.StdEncoding.EncodeToString(value),
					"base64url": base64.RawURLEncoding.EncodeToString(value),
				} {
					if strings.Contains(logged, rendered) {
						t.Errorf("%s appears in the log as %s", what, encoding)
					}
				}
			}

			// The lines that are supposed to be there. A test that only looks for absences
			// passes when the handler logs nothing at all.
			if !strings.Contains(logged, "a server registered") {
				t.Error("the log does not say that a server registered")
			}
			if !strings.Contains(logged, fixtureFingerprint) {
				t.Error("the log does not carry the fingerprint, which is what an operator compares against the game server's own startup line")
			}
		})
	}
}

// **The key is read from a file or from the environment and never from a flag**, because a
// flag is visible in `ps` to every user on the machine and lands in shell history.
//
// Not parallel: t.Setenv and t.Parallel are incompatible, which is the toolchain saying that
// an environment is process-wide state.
func TestTheRegistrationKeyIsReadFromAFileOrTheEnvironment(t *testing.T) {
	path := filepath.Join(t.TempDir(), "registration-key")
	// A trailing newline, which is what `echo key > file` leaves and what an operator would
	// otherwise meet as an authentication failure with nothing in any log to explain it.
	if err := os.WriteFile(path, []byte(fixtureKey+"\n"), 0o600); err != nil {
		t.Fatalf("writing the key file: %v", err)
	}

	fromFile, err := loadRegistrationKey(path, discard())
	if err != nil {
		t.Fatalf("loading the key from a file: %v", err)
	}
	if fromFile == nil || !fromFile.Matches(fixtureKey) {
		t.Error("the key read from a file does not match what was written")
	}

	t.Setenv(registrationKeyEnv, fixtureKey)
	fromEnv, err := loadRegistrationKey("", discard())
	if err != nil {
		t.Fatalf("loading the key from the environment: %v", err)
	}
	if fromEnv == nil || !fromEnv.Matches(fixtureKey) {
		t.Error("the key read from the environment does not match what was set")
	}

	// **Both is a refusal rather than a precedence rule.** A precedence rule is something an
	// operator has to remember; an operator who has set both has already made a mistake
	// worth being told about while both are still true.
	if _, err := loadRegistrationKey(path, discard()); err == nil {
		t.Error("a key given in both places was accepted")
	}
}

// Neither the key nor the file's contents reach an error, and an absent configuration is not
// an error at all: the route answers 503 until an operator invents a key.
func TestAnUnusableRegistrationKeyRefusesAndAnAbsentOneDoesNot(t *testing.T) {
	dir := t.TempDir()

	// Nothing configured. A warning and a nil key, which is the "not configured" state
	// newSignIn already has.
	key, err := loadRegistrationKey("", discard())
	if err != nil {
		t.Fatalf("an unconfigured registration key was an error: %v", err)
	}
	if key != nil {
		t.Error("an unconfigured registration key produced a key")
	}

	// A file that is not there names itself, because a path is not a secret and is the only
	// part of this an operator can act on.
	missing := filepath.Join(dir, "not-here")
	if _, err := loadRegistrationKey(missing, discard()); err == nil || !strings.Contains(err.Error(), missing) {
		t.Errorf("a missing key file answered %v, want an error naming the path", err)
	}

	// A key too short to be one, and the refusal must not quote it.
	weak := filepath.Join(dir, "weak")
	const weakKey = "hunter2"
	if err := os.WriteFile(weak, []byte(weakKey), 0o600); err != nil {
		t.Fatalf("writing the key file: %v", err)
	}
	err = loadKeyErr(t, weak)
	if strings.Contains(err.Error(), weakKey) {
		t.Errorf("the refusal %q quotes the key", err)
	}

	// A key with a line break in the middle cannot be presented in a header at all, so it is
	// refused at startup rather than at every registration.
	broken := filepath.Join(dir, "broken")
	if err := os.WriteFile(broken, []byte("a-registration-key-with-a\nnewline-in-it-somewhere"), 0o600); err != nil {
		t.Fatalf("writing the key file: %v", err)
	}
	if err := loadKeyErr(t, broken); !strings.Contains(err.Error(), "printable ASCII") {
		t.Errorf("a key with a newline answered %v, want the rule stated", err)
	}
}

// loadKeyErr is loadRegistrationKey's error, failing the test if there was not one.
func loadKeyErr(t *testing.T, path string) error {
	t.Helper()

	if _, err := loadRegistrationKey(path, discard()); err != nil {
		return err
	}
	t.Fatalf("the key in %s was accepted", path)
	return nil
}

// A sanity check that the fixture fingerprint is the shape this service accepts, so that a
// test using it is testing the handler rather than the fixture.
func TestTheFixtureFingerprintIsAWellFormedDigest(t *testing.T) {
	t.Parallel()

	for _, fp := range []string{fixtureFingerprint, movedFingerprint} {
		if len(fp) != registry.FingerprintHexLen {
			t.Errorf("the fixture %q is %d characters, want %d", fp, len(fp), registry.FingerprintHexLen)
		}
		if _, err := hex.DecodeString(fp); err != nil {
			t.Errorf("the fixture %q is not hex: %v", fp, err)
		}
	}
}

// **The whole chain, driven end to end, which is what this issue is for.**
//
// An operator registers a game server with their key. A player signs in with Discord and names
// no world, because they have not seen one yet. They read the list with the ticket that
// produced, and it hands them an address to dial and the fingerprint of the certificate that
// server will present — the number `certs.Fingerprint` produces, which is what the client
// checks instead of pinning whatever answered first. Then they sign in again naming the world
// the list told them about, and get a ticket that server will actually accept.
//
// Nothing here reaches into the service for a key: the ticket is verified with what
// `/v1/ticket-key` publishes, because that is the only copy a game server ever has.
func TestTheWholeChainFromSignInToAServerAndItsFingerprint(t *testing.T) {
	t.Parallel()

	fake := newFakeDiscord(t)
	svc, _ := signInService(t, fake, discard())

	// 1. The operator registers their server.
	registerOK(t, svc, aRegistration())

	// 2. The player signs in naming no world at all.
	begun := start(t, svc)
	body, err := json.Marshal(map[string]string{
		"state":         begun.State,
		"code":          fake.issue(t, begun.AuthorizeURL),
		"finish_secret": begun.FinishSecret,
	})
	if err != nil {
		t.Fatalf("building the finish request: %v", err)
	}
	rec := call(t, svc, http.MethodPost, "/v1/signin/discord/finish", string(body))
	if rec.Code != http.StatusOK {
		t.Fatalf("the sign-in answered %d: %s", rec.Code, rec.Body.String())
	}
	var signedIn finishResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &signedIn); err != nil {
		t.Fatalf("reading the finish response: %v", err)
	}

	// 3. And reads the list with the ticket that produced.
	rec = listWith(t, svc, signedIn.SessionTicket)
	if rec.Code != http.StatusOK {
		t.Fatalf("the list answered %d to a freshly signed-in player: %s", rec.Code, rec.Body.String())
	}
	var listed serverListResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &listed); err != nil {
		t.Fatalf("reading the list: %v", err)
	}
	if len(listed.Servers) != 1 {
		t.Fatalf("the list holds %d servers, want 1", len(listed.Servers))
	}
	entry := listed.Servers[0]
	if entry.Address != fixtureAddress {
		t.Errorf("the list gave address %q, want the registered one", entry.Address)
	}
	if entry.CertificateSHA256 != fixtureFingerprint {
		t.Errorf("the list gave fingerprint %q, want the registered one", entry.CertificateSHA256)
	}

	// 4. Naming the world the list told them about gets a ticket that server accepts —
	//    which is the step the account ticket existed to make reachable.
	begun = start(t, svc)
	rec = finishFor(t, svc, begun.State, fake.issue(t, begun.AuthorizeURL), begun.FinishSecret, entry.Name)
	if rec.Code != http.StatusOK {
		t.Fatalf("signing in for the world the list named answered %d: %s", rec.Code, rec.Body.String())
	}
	var joined finishResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &joined); err != nil {
		t.Fatalf("reading the finish response: %v", err)
	}
	minted, err := ticket.Decode(joined.SessionTicket)
	if err != nil {
		t.Fatalf("the ticket in the response is not a ticket: %v", err)
	}

	world, err := ticket.WorldIDFor(entry.Name)
	if err != nil {
		t.Fatalf("the list named a world no ticket can be minted for: %v", err)
	}
	// Verified exactly as the game server will: the published key, that server's own world.
	if _, err := ticket.Verify(publishedKey(t, svc), minted[:], world, time.Now()); err != nil {
		t.Fatalf("the game server named by the list would have refused the ticket: %v", err)
	}

	// And the account ticket from step 2 is still not a way into that game.
	account, err := ticket.Decode(signedIn.SessionTicket)
	if err != nil {
		t.Fatalf("the account ticket is not a ticket: %v", err)
	}
	if _, err := ticket.Verify(publishedKey(t, svc), account[:], world, time.Now()); err == nil {
		t.Error("the game server accepted the account ticket, which is the one thing it must not do")
	}
}
