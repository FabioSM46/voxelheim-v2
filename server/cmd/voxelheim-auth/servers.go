package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/registry"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// maxRegistrationRequestBytes bounds the registration body.
//
// The four fields together cannot exceed about four hundred bytes, so four kilobytes is
// far more than one needs. It is the same bound the sign-in requests carry and for the
// same reason: a POST is a way to hand this process an arbitrary amount of JSON to decode,
// and this one is reachable before the key has been checked, because the body has to be
// read to be refused.
const maxRegistrationRequestBytes = 4 << 10

// The refusal vocabulary these two endpoints answer with, in the closed-set shape the
// sign-in routes established. Each is a distinction the caller acts on.
//
// **These are the two routes in this service that answer 401, and the reason is that they
// are the two with an authentication scheme.** signin.go says in as many words that every
// refusal there is a 400 and none is a 401, because "a 401 without a WWW-Authenticate
// header is not what that status means, and inventing an authentication scheme to satisfy
// it would be inventing a scheme". Here there is no invention: a registration presents
// `Authorization: Bearer <registration key>` and a list read presents
// `Authorization: Bearer <session ticket>`, so `WWW-Authenticate: Bearer` is true and the
// 401 says what a 400 could not — that the request would be fine with a credential.
const (
	// This deployment was never given a registration key. Its own code rather than
	// errUnauthorized, and a 503 rather than a 401, because no credential would work: it
	// is a service saying what is missing rather than one refusing what it was shown.
	errRegistrationNotConfigured = "registration_not_configured"

	// No credential, or one that is not this service's. **One answer for every way of
	// getting it wrong** — absent, malformed, wrong scheme, wrong key, a ticket nobody
	// signed — because distinguishing them tells whoever is guessing which guesses are
	// getting warmer. internal/discord answers unknown, expired and already-redeemed as
	// one refusal for exactly this reason.
	errUnauthorized = "unauthorized"

	// The one refusal deliberately split out of errUnauthorized: a good ticket that has
	// run out. It leaks nothing — whoever holds a ticket already holds its expiry, signed
	// — and it is the difference between a client sending its player back to the login
	// screen with a line saying why and one showing them a failure to interpret. There is
	// no revocation in this design, so this is the only way a ticket ever stops working.
	errTicketExpired = "ticket_expired"

	// The four field refusals a registration can get. **Split by field on purpose**, which
	// is the opposite of the rule the sign-in routes keep, because the callers are
	// opposites: a sign-in is refused to somebody unauthenticated who is told as little as
	// possible, and a registration is refused to an operator holding this service's own key
	// who has one configuration to fix and needs to be told which line. The value that was
	// wrong is never echoed back — the code names the field, and internal/registry's own
	// messages, which do quote most fields, stay in this service's log.
	errServerNotNamed        = "server_not_named"
	errDisplayNameRefused    = "display_name_refused"
	errAddressRefused        = "address_refused"
	errFingerprintNotADigest = "fingerprint_not_a_digest"

	// The registry could not be read or written. A 500: nothing about the request caused
	// it, and the thing to do about it is look at this service's disk.
	errRegistryUnavailable = "registry_unavailable"
)

// registerRequest is what a game server announces about itself.
//
// Every field is replaced whole by the next announcement — see registry.Store.Register.
// There is nothing to merge, because a record is the last thing a server said.
type registerRequest struct {
	// Name is the server's identifier **and the world name a ticket is minted for**. It is
	// what the client reads out of the list and hands to
	// `POST /v1/signin/discord/finish`, which is how one string closes the trust chain
	// rather than two that have to be kept in step.
	Name string `json:"name"`

	// DisplayName is the title a player reads. Absent, it becomes the name — see
	// registerServer, where that default is applied, rather than in the store.
	DisplayName string `json:"display_name"`

	// Address is where a player connects, as host:port. **It is separate from whatever the
	// game server is listening on**: a server bound to every interface has to announce
	// something a player can actually reach.
	Address string `json:"address"`

	// CertificateSHA256 is the fingerprint of the certificate the game server presents,
	// lowercase hex — the number `certs.Fingerprint` produces and `voxelheimd` logs at
	// startup as `certificate_sha256=…`.
	//
	// Named for the algorithm rather than called "fingerprint", for the reason
	// `/v1/ticket-key` publishes `algorithm` beside its key: a reader is told what the
	// bytes are instead of inferring it from their length, and a future digest is a field
	// that changes rather than a silent reinterpretation of this one.
	CertificateSHA256 string `json:"certificate_sha256"`
}

// registerResponse tells an announcer what the registry now holds for it.
type registerResponse struct {
	Name string `json:"name"`

	// Created reports whether this name had never been registered before. Useful to an
	// operator watching a first announce succeed, and to nothing else: a re-registration
	// is the ordinary case and is not an event.
	Created bool `json:"created"`

	// OfflineAfterSeconds is how long this server may now go quiet before the list shows it
	// as offline.
	//
	// **Published so the announcing side has a number to be under rather than a number to
	// pick.** An announce interval chosen independently of this one is two constants that
	// eventually disagree, and the failure is a healthy server flapping to offline in front
	// of players.
	OfflineAfterSeconds int `json:"offline_after_seconds"`
}

// serverListEntry is one row of the list a player chooses from.
type serverListEntry struct {
	// Name is what the client hands back to `finish` to be minted a ticket for this world.
	Name string `json:"name"`

	// DisplayName is what a player reads.
	DisplayName string `json:"display_name"`

	// Address is what the client dials.
	Address string `json:"address"`

	// CertificateSHA256 is **the expectation that replaces trust on first use**: the client
	// refuses a server whose certificate is not this one, and the number comes from here
	// rather than from whatever answered the first time it connected.
	CertificateSHA256 string `json:"certificate_sha256"`

	// Online reports whether this server has been heard from inside registry.OfflineAfter.
	// It is not a reachability probe — nothing in this service dials anybody — it is "this
	// server said something recently".
	Online bool `json:"online"`

	// LastSeen is when it last announced, so a client can say "quiet for two hours" rather
	// than only "offline".
	LastSeen time.Time `json:"last_seen"`
}

// serverListResponse is the whole list.
type serverListResponse struct {
	// Servers is ordered by name and is **never null**: an empty registry answers `[]`, so
	// a client decoding this always has a list to iterate. A null here would be a second
	// shape for "no servers" that every consumer has to handle separately.
	Servers []serverListEntry `json:"servers"`

	// OfflineAfterSeconds is the window Online was computed against, published so a client
	// showing "last seen" can say what the threshold was.
	OfflineAfterSeconds int `json:"offline_after_seconds"`
}

// registerServer records where a game server is and what certificate it presents.
//
// **This is the endpoint the security of the whole list rests on.** Whoever can reach it
// with the key can put an address in the list under a name players trust, and a client
// that trusts the list will connect to it and accept the certificate it was told to
// expect. So the key is checked first, before the body is even decoded: a caller who
// cannot register should not be able to make this process parse anything.
func (s *service) registerServer(w http.ResponseWriter, r *http.Request) {
	if s.registrationKey == nil {
		// Not an authentication failure — no key would work — so it is not a 401. See the
		// vocabulary above.
		s.refuse(w, http.StatusServiceUnavailable, errRegistrationNotConfigured)
		return
	}
	presented, ok := bearerCredential(r)
	if !ok || !s.registrationKey.Matches(presented) {
		// **The two failures are one answer**, and neither is logged with anything from
		// the request. A wrong key is somebody guessing or an operator who typed it wrong,
		// and the log line has to be readable when it is the second without being useful
		// when it is the first.
		s.log.Info("a server registration was refused", "reason", "no valid registration key was presented")
		s.refuseUnauthorized(w, errUnauthorized)
		return
	}

	var req registerRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxRegistrationRequestBytes)).Decode(&req); err != nil {
		// The decode error is not logged. It is built from the request body, and
		// encoding/json quotes bytes of the offending input in some of its messages — a
		// registration body carries an address, which is the one value in this service
		// that must not reach a log.
		s.log.Info("a server registration could not be read")
		s.refuse(w, http.StatusBadRequest, errMalformedRequest)
		return
	}

	displayName := req.DisplayName
	if displayName == "" {
		// **The default lives here rather than in the store**, because "the field was
		// absent" is a fact about a request and the store never sees one. It also means
		// there is exactly one place that decides it, so a record and a response can never
		// disagree about what a server is called.
		displayName = req.Name
	}

	srv := registry.Server{
		Name:        req.Name,
		DisplayName: displayName,
		Address:     req.Address,
		// Named exactly what the request field is, which is what the fingerprint the client
		// receives will be called too: one number with one name from the game server's log
		// line to the client's refusal message.
		Fingerprint: req.CertificateSHA256,
		// The registry's own clock, not the announcer's. An announcer that could set this
		// could claim to have been heard from at any moment it liked, and "heard from
		// recently" would stop being something this service knows.
		LastSeen: time.Now(),
	}

	created, err := s.servers.Register(srv)
	if err != nil {
		if code, refused := registrationRefusalCode(err); refused {
			// The reason is logged in full and the caller is given the field. The message
			// is internal/registry's, which quotes every field but the address.
			s.log.Info("a server registration was refused", "server", req.Name, "error", err)
			s.refuse(w, http.StatusBadRequest, code)
			return
		}
		s.log.Error("a server registration could not be recorded", "server", req.Name, "error", err)
		s.refuse(w, http.StatusInternalServerError, errRegistryUnavailable)
		return
	}

	// **The address is deliberately absent from this line**, and it is the only field that
	// is. It locates somebody's house — which is the whole reason the list below is behind
	// a credential — and a value that must not be published is a value that must not be in
	// a log either. The name and the fingerprint are both public by construction: the
	// fingerprint is in the game server's own startup line, and comparing the two is how an
	// operator checks that the right certificate reached the list.
	s.log.Info("a server registered",
		"server", srv.Name,
		"created", created,
		"certificate_sha256", srv.Fingerprint)

	s.writeJSON(w, http.StatusOK, registerResponse{
		Name:                srv.Name,
		Created:             created,
		OfflineAfterSeconds: int(registry.OfflineAfter.Seconds()),
	})
}

// listServers answers the list a player chooses from.
//
// **Readable only by an authenticated account**, which is what keeps it from being a public
// directory of people's home addresses. Any ticket this service signed will do — an account
// ticket, or one scoped to a world — because all this endpoint needs to know is that
// somebody signed in. It is deliberately not scoped to a world: the list is what tells a
// player which worlds there are, so a credential naming one would close the chain in a
// circle. See ticket.VerifyAnyWorld, which is the only caller of that function and says so.
func (s *service) listServers(w http.ResponseWriter, r *http.Request) {
	claims, ok := s.authenticateReader(w, r)
	if !ok {
		return
	}

	servers, err := s.servers.List()
	if err != nil {
		// A record that cannot be read fails the whole list rather than quietly shortening
		// it — registry.Store.List carries the argument. From here it is a 500, because
		// nothing about this request caused it.
		s.log.Error("the server list could not be read", "error", err)
		s.refuse(w, http.StatusInternalServerError, errRegistryUnavailable)
		return
	}

	now := time.Now()
	entries := make([]serverListEntry, 0, len(servers))
	for _, srv := range servers {
		entries = append(entries, serverListEntry{
			Name:              srv.Name,
			DisplayName:       srv.DisplayName,
			Address:           srv.Address,
			CertificateSHA256: srv.Fingerprint,
			Online:            srv.Online(now),
			LastSeen:          srv.LastSeen,
		})
	}

	// Debug rather than info: a client may read this on every launch and a menu may refresh
	// it, so a line per read would drown the two events in this service that are worth
	// noticing — a sign-in and a registration. The account id is safe to log by
	// construction; it is minted at random and describes nobody.
	s.log.Debug("the server list was read", "account_id", claims.Account.String(), "servers", len(entries))

	s.writeJSON(w, http.StatusOK, serverListResponse{
		Servers:             entries,
		OfflineAfterSeconds: int(registry.OfflineAfter.Seconds()),
	})
}

// authenticateReader answers who is asking, or refuses and reports that it has.
//
// It writes the refusal itself so that every reader of this list is admitted by one piece
// of code: a second copy of these branches is a second chance for one of them to be a
// `return` that forgot to refuse, which is an endpoint that answers everybody.
func (s *service) authenticateReader(w http.ResponseWriter, r *http.Request) (ticket.Claims, bool) {
	presented, ok := bearerCredential(r)
	if !ok {
		s.refuseUnauthorized(w, errUnauthorized)
		return ticket.Claims{}, false
	}
	raw, err := ticket.Decode(presented)
	if err != nil {
		s.refuseUnauthorized(w, errUnauthorized)
		return ticket.Claims{}, false
	}

	// The public half of this service's own pair, which is the same key a game server reads
	// from /v1/ticket-key. Nothing here holds a second idea of what a valid ticket is.
	claims, err := ticket.VerifyAnyWorld(s.keys.Public(), raw[:], time.Now())
	switch {
	case errors.Is(err, ticket.ErrExpired):
		s.refuseUnauthorized(w, errTicketExpired)
		return ticket.Claims{}, false
	case err != nil:
		// **Never logged with the ticket in it.** A ticket that failed to verify is still
		// somebody's bearer credential — a real one, copied and edited, is exactly the case
		// this branch exists for — and internal/ticket's own errors are careful never to
		// quote the bytes.
		s.log.Info("a server list read was refused", "reason", "the ticket did not verify")
		s.refuseUnauthorized(w, errUnauthorized)
		return ticket.Claims{}, false
	}
	return claims, true
}

// registrationRefusalCode maps a store refusal onto the code the caller is given, and
// reports whether it was a refusal at all.
//
// The mapping is on internal/registry's sentinels rather than on any string, which is the
// whole reason that package has four of them: a field-by-field answer built by matching
// error text is one that changes meaning the next time somebody improves a message.
//
// A false answer is deliberately not an "unknown refusal" code. Anything that is not one of
// these is the disk, and the caller is told so with a 500 rather than with a 400 naming a
// field that was fine.
func registrationRefusalCode(err error) (string, bool) {
	switch {
	case errors.Is(err, registry.ErrServerName):
		return errServerNotNamed, true
	case errors.Is(err, registry.ErrDisplayName):
		return errDisplayNameRefused, true
	case errors.Is(err, registry.ErrAddress):
		return errAddressRefused, true
	case errors.Is(err, registry.ErrFingerprint):
		return errFingerprintNotADigest, true
	}
	return "", false
}

// bearerCredential is the credential in an `Authorization: Bearer …` header.
//
// The scheme name is matched case-insensitively, which RFC 7235 requires, and the
// credential is taken verbatim after exactly one space. Nothing is trimmed: a credential
// with whitespace around it is not this service's credential, and quietly accepting one
// would mean two spellings of a key — the thing registry.ParseKey refuses at startup so
// that it can never be true at a comparison.
//
// A missing header, a header with another scheme and an empty credential are one answer,
// and the caller turns all of them into the same refusal.
func bearerCredential(r *http.Request) (string, bool) {
	const scheme = "bearer "

	header := r.Header.Get("Authorization")
	if len(header) <= len(scheme) || !strings.EqualFold(header[:len(scheme)], scheme) {
		return "", false
	}
	return header[len(scheme):], true
}

// refuseUnauthorized answers a 401 with the header that makes it one.
//
// **`WWW-Authenticate` is what a 401 means**, and omitting it would leave this service
// using the status as a synonym for "no" — which is the reason signin.go declines to use
// it at all. The challenge names the scheme and nothing else: a realm would be a string
// this service invents for a client that has nowhere to show it.
func (s *service) refuseUnauthorized(w http.ResponseWriter, code string) {
	w.Header().Set("WWW-Authenticate", "Bearer")
	s.refuse(w, http.StatusUnauthorized, code)
}

// registrationKeyEnv is where the registration key is read from when no file names it.
const registrationKeyEnv = "VOXELHEIM_REGISTRATION_KEY"

// loadRegistrationKey reads the operator's registration key, or reports that this
// deployment has none.
//
// **A nil key and a nil error is "not configured"**, the deliberate state newSignIn already
// has and for the same reason: this is a value an operator invents, and refusing to start
// without one would mean the account service could not run at all until somebody had —
// including in every test that is about the store, the port or the health probe. So the
// route exists either way and answers 503 until a key is given, which is a service that
// says what is missing rather than one that is silently absent. Nothing else is affected:
// the list is read with a ticket, so it works with no registration key at all — it is
// simply empty until something can register.
//
// **The key is read from a file or from the environment, and never from a flag.** A flag is
// visible in `ps` to every user on the machine and lands in shell history; a file's path
// and an environment variable's name are not the secret. The two sources are mutually
// exclusive rather than ordered: a precedence rule is something an operator has to
// remember, and an operator who has set both has already made a mistake worth being told
// about while both are still true.
//
// Whatever is read is trimmed of surrounding whitespace by registry.ParseKey, because
// `echo key > key-file` leaves a newline and an operator who had to notice that would
// notice it as an authentication failure with nothing in any log to explain it.
func loadRegistrationKey(path string, log *slog.Logger) (*registry.Key, error) {
	fromEnv, inEnv := os.LookupEnv(registrationKeyEnv)
	named := strings.TrimSpace(path)

	var raw string
	switch {
	case inEnv && named != "":
		return nil, fmt.Errorf("the registration key is given both in %s and in the file named by -registration-key-file (%s); "+
			"give it in exactly one of the two", registrationKeyEnv, named)

	case inEnv:
		raw = fromEnv

	case named != "":
		// The path is named in the error and the contents never are. A path is not a
		// secret, and it is the only part of this an operator can act on.
		data, err := os.ReadFile(named)
		if err != nil {
			return nil, fmt.Errorf("reading the registration key from %s: %w", named, err)
		}
		raw = string(data)

	default:
		log.Warn("server registration is not configured; POST /v1/servers will refuse every request",
			"flag", "-registration-key-file", "env", registrationKeyEnv)
		return nil, nil
	}

	key, err := registry.ParseKey(raw)
	if err != nil {
		// registry.ParseKey never quotes the key, so neither does this.
		return nil, err
	}
	log.Info("server registration configured", "offline_after", registry.OfflineAfter)
	return &key, nil
}
