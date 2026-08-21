package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"
)

// The account service's registration route, and the shape of what this server sends and
// reads there.
//
// Stated here rather than shared with cmd/voxelheim-auth, for the reason ticketKeyPath is
// stated in tickets.go: that command is the account service, this one is a game server,
// and they are two programs that meet over HTTP. A shared struct would be an import from
// one service into the other — and internal/registry/imports_test.go forbids exactly that
// import, because a registry record holds the address of somebody's house and this process
// has no business opening that directory. What keeps the two in step is the field names
// below and the tests that spell them out.
const serversPath = "/v1/servers"

// registrationKeyEnv is where the registration key is read from when no file names it.
//
// **The same variable cmd/voxelheim-auth reads, deliberately.** It is one secret with two
// ends — the account service holds its digest to compare against, this server holds it to
// present — and giving each end its own name would be two names for one value, which is
// one more thing to get wrong on a machine that runs both.
const registrationKeyEnv = "VOXELHEIM_REGISTRATION_KEY"

// What one announcement is allowed to cost, and how often one is made.
const (
	// announceEvery is how often this server restates where it is, before the account
	// service has told it what it may be.
	//
	// A minute against the five minutes internal/registry.OfflineAfter documents: four
	// tries inside the window, so one missed announce — a dropped packet, an account
	// service restart, a home connection blinking — cannot flap a healthy server to
	// offline in front of players. **The five is not copied here**; see [announcer.settle],
	// which takes the number out of the acknowledgement instead.
	announceEvery = time.Minute

	// minAnnounceEvery is the floor under an interval derived from the acknowledgement.
	//
	// It bounds nothing an operator configures — only what a *service* can talk this
	// process into. An account service publishing a four-second window would otherwise
	// derive a one-second interval, and "answering nonsense" would become a hot loop
	// against somebody else's machine.
	minAnnounceEvery = 5 * time.Second

	// announceTriesPerWindow is how many announcements have to fit inside the window the
	// account service publishes. See [announcer.settle].
	announceTriesPerWindow = 4

	// maxOfflineAfterSeconds is the largest window this server will believe. A day is far
	// past anything internal/registry would choose and still small enough that the
	// arithmetic below cannot overflow a duration.
	maxOfflineAfterSeconds = 24 * 60 * 60

	// announceTimeout bounds one announcement, so that a service which accepts a
	// connection and then says nothing costs one interval rather than the life of the
	// process. The context it derives from is the shutdown one, so a ctrl-C during a stalled
	// announce still ends the server at once.
	announceTimeout = 10 * time.Second

	// maxAnnounceResponseBytes bounds what is read from the acknowledgement before
	// anything is parsed. The ordering is the point, and it is tickets.go's rule one
	// endpoint over: the answer is as long as whoever answered chose, and the three fields
	// this server reads are a hundred bytes of JSON.
	maxAnnounceResponseBytes = 4096
)

// redactedRegistrationKey is what a [registrationKey] renders as, whichever formatter
// reaches it.
const redactedRegistrationKey = "main.registrationKey(redacted)"

// registrationKey is the operator-configured credential this server registers with.
//
// **It is not a player credential and it is a credential.** Whoever holds it can put an
// address in the account service's list under a name players trust, which is a better
// attack than the one that list replaces, because a client that trusts the list believes
// the answer. So it comes from the environment or from a file and never from a flag — a
// flag value is in `ps` for every user on the machine and in the operator's shell history —
// and it never reaches a log.
//
// Unlike `registry.Key`, which keeps only a digest because it only ever has to *compare*,
// this end has to *present* the key, so the bytes are here. The type is what keeps them out
// of the log instead, in the shape `discord.Secret` and `identity.Account` established, and
// the four methods are deliberately four because each covers a route the others do not:
//
//   - [registrationKey.String] covers fmt, and therefore %v, %s, %q and every error message
//     built with fmt.Errorf.
//   - [registrationKey.GoString] covers %#v, which a Stringer never sees.
//   - [registrationKey.LogValue] covers log/slog, which resolves a LogValuer before either
//     handler formats anything. Without it, -log-format json would hand the value to
//     encoding/json and write it out verbatim.
//   - [registrationKey.MarshalJSON] covers a struct that happens to hold one being
//     marshalled into a request body or a diagnostic — which is not hypothetical here,
//     because this server marshals a struct to JSON on every announcement.
type registrationKey string

// Reveal is the value itself, and it is a named method so that every place the key escapes
// the type is one grep away. There is exactly one such place: the `Authorization` header in
// [announcer.post].
func (k registrationKey) Reveal() string { return string(k) }

// IsEmpty reports whether this key holds nothing, without revealing what it holds.
func (k registrationKey) IsEmpty() bool { return k == "" }

// String redacts the key, for fmt and for every error message built through it.
func (k registrationKey) String() string { return redactedRegistrationKey }

// GoString redacts a key printed with %#v, which String never sees.
func (k registrationKey) GoString() string { return redactedRegistrationKey }

// LogValue redacts a key that reaches a log line. Not the same defence as String: slog
// resolves a LogValuer before either handler formats anything, and the text handler formats
// a struct through fmt.
func (k registrationKey) LogValue() slog.Value { return slog.StringValue(redactedRegistrationKey) }

// MarshalJSON redacts a key that reaches encoding/json.
func (k registrationKey) MarshalJSON() ([]byte, error) { return json.Marshal(redactedRegistrationKey) }

// announcement is what this server says about itself, in the three fields
// `POST /v1/servers` reads.
//
// Every field is replaced whole by the next announcement, which is the point rather than a
// convenience: the address the list serves is the one this server last announced, so a home
// connection that gets a new address overnight stops being invisible to players.
type announcement struct {
	// Name is this world's name — the same string a session ticket is minted for, which is
	// what lets a client read a name out of the list and hand it straight back to the
	// account service. There is deliberately no display name here: the account service
	// defaults it to this, and a second title is a value this issue was not asked for.
	Name string `json:"name"`

	// Address is where a player connects, as host:port. **Separate from -listen**, because
	// a server bound to every interface has to announce something a player can reach.
	Address string `json:"address"`

	// CertificateSHA256 is the fingerprint of the certificate this server actually
	// presents: `certs.Fingerprint`'s number, taken from the certificate `listen` loaded,
	// and the same 64 characters the startup line carries as `certificate_sha256=…`.
	// Nothing here computes a digest — a second way of arriving at this number is a second
	// number for a client to disagree with.
	CertificateSHA256 string `json:"certificate_sha256"`
}

// registration is the part of the acknowledgement this server reads.
type registration struct {
	Name string `json:"name"`

	// Created reports whether this name had never been registered before. Worth one log
	// line to an operator watching a first announce land, and nothing after that.
	Created bool `json:"created"`

	// OfflineAfterSeconds is how long this server may now go quiet before the list shows it
	// as offline. **Read rather than assumed**: it is published so that the announcing side
	// has a number to be under instead of a second constant that eventually disagrees with
	// `registry.OfflineAfter`. See [announcer.settle].
	//
	// An int64 rather than an int so that what a 32-bit build makes of a large number is
	// the same as what a 64-bit one does; the range check is in settle, not in the decoder.
	OfflineAfterSeconds int64 `json:"offline_after_seconds"`
}

// refusal is the closed-set code the account service answers a refused registration with.
type refusal struct {
	Error string `json:"error"`
}

// knownRefusals is the vocabulary cmd/voxelheim-auth answers with, and the only text from a
// response body this server will put in its own log.
//
// **A response body is a third party's text**: whatever answered that address is not known
// to be the account service, so echoing its bytes into this server's log would be letting a
// stranger write there. Matching against a closed set is what makes the one genuinely useful
// case — an operator being told that it is the *address* that was refused rather than the
// key — safe to keep.
var knownRefusals = map[string]bool{
	"registration_not_configured": true,
	"unauthorized":                true,
	"malformed_request":           true,
	"server_not_named":            true,
	"display_name_refused":        true,
	"address_refused":             true,
	"fingerprint_not_a_digest":    true,
	"registry_unavailable":        true,
}

// errAnnounceNotConfigured reports that nobody asked this server to announce itself.
//
// A distinct sentinel because it is the one "no announcer" answer that is not a mistake, and
// the difference is a log level: a server nobody asked to join a list says so once at Info,
// and a half-configured one is warned about.
var errAnnounceNotConfigured = errors.New("nothing has asked this server to announce itself")

// announcer tells the account service where this server is, repeatedly, and never stops the
// server from running.
//
// The fields below are settled once, at startup, by [newAnnouncer]; every mutable one is
// touched only by [announcer.loop]'s own goroutine, so there is no lock here and none is
// needed.
type announcer struct {
	// endpoint is the registration URL as the request needs it, userinfo and all; redacted
	// is the same URL as it is written down. The split is tickets.go's and it is the reason
	// a password an operator wrote into -account-service stops at this process.
	endpoint *url.URL
	redacted string

	key         registrationKey
	name        string
	address     string
	fingerprint string

	// every is the interval between announcements, and timeout bounds one of them. Both are
	// fields rather than constants so that a test can shorten them; every is also what
	// [announcer.settle] narrows when the account service publishes a tighter window.
	every   time.Duration
	timeout time.Duration

	client *http.Client
	log    *slog.Logger

	// What the last pass did, so that a service which has been unreachable for an hour is
	// one warning rather than sixty. Read and written only by loop's goroutine.
	announced   bool
	lastFailure string
}

// render is what an announcer renders as, and it carries the two values that are safe to
// write down and neither of the two that are not.
//
// **A type composed of redacting types is not itself redacted, and this is the outer type
// saying so** — `ticket.Pair` had to learn it twice and this is the third time it applies.
// fmt reaches a Stringer only through a value it could hand to an interface, and
// `reflect.Value.CanInterface` is false for an **unexported field**, so the reflection walker
// steps straight past every method [registrationKey] declares and prints the key inside it.
// One `%+v` of this struct — in a diagnostic, in an error, in a log line somebody added while
// chasing something else — is the whole credential in the log, and the address beside it.
//
// **Every rendering method below takes a value receiver**, because a method set on `*T` leaves
// a `T` value implementing neither fmt.Stringer nor slog.LogValuer, which a caller reaches by
// nothing more exotic than a dereference. A nil `*announcer` is the ordinary state of this
// field and would panic in the dereference — fmt and slog both recover from that and print
// their own placeholder, which is the right answer and not one worth a branch here.
func (a announcer) render() string {
	return fmt.Sprintf("main.announcer(world %s at %s)", a.name, a.redacted)
}

// String redacts the announcer, for fmt and for every error message built through it.
func (a announcer) String() string { return a.render() }

// GoString redacts an announcer printed with %#v, which String never sees. This is the route
// ticket.Pair's own leak took.
func (a announcer) GoString() string { return a.render() }

// LogValue redacts an announcer that reaches a log line. Not the same defence as String: slog
// resolves a LogValuer before either handler formats anything.
func (a announcer) LogValue() slog.Value { return slog.StringValue(a.render()) }

// MarshalJSON redacts an announcer that reaches encoding/json — the fourth route, and the one
// -log-format json would otherwise take.
func (a announcer) MarshalJSON() ([]byte, error) { return json.Marshal(a.render()) }

// newAnnouncer settles whether this server announces itself, and says so exactly once.
//
// **It never refuses a start, and that is the whole point of the issue this implements.**
// Admitting a player is a signature check precisely so that the account service being down
// costs nobody a game; an announcer that could take the server down over a call to that same
// service would undo it in one line. So every way of getting this wrong — no configuration,
// half a configuration, a key that cannot be presented, an address no player could dial —
// ends in a nil announcer and a server that serves.
//
// **What it does refuse is silence.** A misconfiguration is one line at Warn naming the flag
// or the variable to fix, and it is a startup line rather than a complaint per interval:
// nothing here is retried, because nothing here can change while the process runs.
func newAnnouncer(opts options, fingerprint string, log *slog.Logger) *announcer {
	a, err := configureAnnouncer(opts, fingerprint)
	switch {
	case errors.Is(err, errAnnounceNotConfigured):
		// **One clean line, not a warning per interval.** Not announcing is an ordinary way
		// to run a server — a LAN game, a test, an operator who has not registered one — so
		// this says what is off and what would turn it on, and never mentions it again.
		log.Info("this server will not announce itself to any account service; it will not appear in the list players choose from",
			"account_service_flag", "-account-service",
			"announce_address_flag", "-announce-address",
			"registration_key_env", registrationKeyEnv,
			"registration_key_flag", "-registration-key-file")
		return nil

	case err != nil:
		// Half configured, or configured with something that cannot work. Louder than the
		// case above because somebody meant to be in the list and will not be — and still
		// not fatal, for the reason in this function's comment.
		log.Warn("this server will not announce itself; the game runs and it will not appear in the list players choose from",
			"error", err)
		return nil
	}

	a.log = log
	// The address is deliberately absent from this line, alone among the fields, and it is
	// the same rule internal/registry keeps for the same value: it locates somebody's house.
	// The operator typed it, `-h` documents it, and nothing is gained by writing it down.
	// The fingerprint beside it is public by construction — it is a hash of the certificate
	// this server hands to everyone who connects — and having it here is what lets an
	// operator compare one string against what the list ends up serving.
	log.Info("announcing this server to the account service",
		"endpoint", a.redacted,
		"world_name", a.name,
		"certificate_sha256", a.fingerprint,
		"every", a.every.String())
	return a
}

// configureAnnouncer builds an announcer out of the flags and the environment, or reports
// why there is none.
//
// Separate from [newAnnouncer] so that the decision is a pure function of its inputs and the
// logging is somewhere else: what a test wants to assert is which configurations announce,
// and that is an error value rather than a line of text.
//
// **Validated before it is used, the rule options.validate keeps for every other flag.** The
// difference is only what a failure costs: a tick rate out of range refuses a start because a
// server cannot run without one, and an announce address that is not host:port disables an
// announcer because a server runs perfectly well without one. Checking it here rather than
// letting the account service answer 400 every minute is what turns a repeated refusal
// nobody reads into one startup line naming the flag.
func configureAnnouncer(opts options, fingerprint string) (*announcer, error) {
	key, err := registrationKeyFor(opts.registrationKeyFile)
	if err != nil {
		return nil, err
	}

	address := strings.TrimSpace(opts.announceAddress)
	service := strings.TrimSpace(opts.accountService)

	// Three things are needed and none of them can be guessed: where the account service is,
	// what proves this server may register, and what address to announce — which is not the
	// listen address and cannot be derived from it, because a server on 0.0.0.0 is reachable
	// at an address only its operator knows.
	switch {
	case service == "" && key.IsEmpty() && address == "":
		return nil, errAnnounceNotConfigured
	case service == "":
		return nil, errors.New("this server has a registration key or an announce address but no -account-service to " +
			"announce to; give it the account service's base URL, or unset the other two")
	case key.IsEmpty():
		return nil, fmt.Errorf("this server has an announce address but no registration key; put one in %s or in the "+
			"file named by -registration-key-file. It is never taken from a flag, because a flag value is visible in "+
			"`ps` to every user on this machine", registrationKeyEnv)
	case address == "":
		return nil, errors.New("this server has a registration key but no -announce-address; a server announces the " +
			"address players dial, which is not necessarily the one it listens on")
	}

	base, err := parseAccountService(service)
	if err != nil {
		return nil, err
	}
	if err := validateAnnounceAddress(address); err != nil {
		return nil, err
	}
	if fingerprint == "" {
		// Unreachable: listen computes it from the certificate it just loaded and fails
		// rather than returning an empty one. Kept because an announcement without it is a
		// row in the list that every client refuses to connect to, which is worse than no row.
		return nil, errors.New("this server has no certificate fingerprint to announce")
	}

	endpoint := base.JoinPath(serversPath)
	return &announcer{
		endpoint:    endpoint,
		redacted:    endpoint.Redacted(),
		key:         key,
		name:        opts.worldName,
		address:     address,
		fingerprint: fingerprint,
		every:       announceEvery,
		timeout:     announceTimeout,
		client:      http.DefaultClient,
	}, nil
}

// registrationKeyFor reads the operator's registration key, or answers an empty one.
//
// **From a file or from the environment, and never from a flag** — cmd/voxelheim-auth's rule
// at the other end of the same secret, and here for the same reason: a flag lands in `ps` for
// every user on the machine and in the operator's shell history, while a path and a variable
// name are not the secret.
//
// The two sources are mutually exclusive rather than ordered, because a precedence rule is
// something an operator has to remember and one who has set both has already made a mistake
// worth being told about while both are still true.
//
// Surrounding whitespace is trimmed, because `echo key > key-file` leaves a newline and an
// operator who had to notice that would notice it as a 401 with nothing in any log to explain
// it. **Both ends trim**, which is what makes that safe: registry.ParseKey does the same to
// the key it compares against.
func registrationKeyFor(path string) (registrationKey, error) {
	fromEnv, inEnv := os.LookupEnv(registrationKeyEnv)
	named := strings.TrimSpace(path)

	switch {
	case inEnv && named != "":
		// The variable and the path are named; neither is a secret. The value never is.
		return "", fmt.Errorf("the registration key is given both in %s and in the file named by "+
			"-registration-key-file (%s); give it in exactly one of the two", registrationKeyEnv, named)

	case inEnv:
		return parseRegistrationKey(fromEnv)

	case named != "":
		data, err := os.ReadFile(named)
		if err != nil {
			// The path is named and the contents never are. A path is not a secret, and it is
			// the only part of this an operator can act on.
			return "", fmt.Errorf("reading the registration key from %s: %w", named, err)
		}
		return parseRegistrationKey(string(data))
	}
	return "", nil
}

// parseRegistrationKey checks the one thing this end of the secret can check, and never
// quotes it.
//
// **The length rules belong to the account service, not here.** internal/registry.MinKeyBytes
// is what an operator is held to and this process may not import that package — the boundary
// test in internal/registry says so, because a registry record holds the address of somebody's
// house. Restating the number would be a copy that drifts, and it would refuse at the wrong
// end: the service refuses to *start* with a key that is too short, so a key this side would
// have rejected is one there is no service to present it to.
//
// What is checked is what this end owns: the key is presented in an `Authorization` header,
// which is bytes on one line. A key carrying a newline, a tab or a space cannot be presented
// at all — net/http refuses to write it — so a startup line naming the rule is the difference
// between a message an operator can act on and every announcement failing for no stated
// reason. The commonest cause is a key file that picked up a line break when it was pasted,
// which trimming the ends does not fix.
func parseRegistrationKey(raw string) (registrationKey, error) {
	key := strings.TrimSpace(raw)
	switch {
	case key == "":
		return "", fmt.Errorf("the registration key in %s or in the file named by -registration-key-file is empty",
			registrationKeyEnv)
	case !printableASCIIKey(key):
		return "", errors.New("a registration key is printable ASCII with no spaces, and this one is not; " +
			"the usual cause is a line break picked up when it was pasted into the file")
	}
	return registrationKey(key), nil
}

// printableASCIIKey reports whether s is entirely printable ASCII with no spaces: every byte
// in 0x21..0x7e. internal/registry's own rule for the same value, restated because that
// package may not be imported here — a predicate rather than a threshold, so there is no
// number to drift.
func printableASCIIKey(s string) bool {
	for i := 0; i < len(s); i++ {
		if s[i] < 0x21 || s[i] > 0x7e {
			return false
		}
	}
	return true
}

// validateAnnounceAddress reports whether addr is somewhere a player could actually be sent.
//
// It is host:port with a numeric port, which is internal/registry's rule for the field it
// stores and cmd/voxelheim-auth's for -listen: a named port resolves against whatever
// /etc/services says on the machine that reads it, and a client on another machine would dial
// somewhere else. Nothing is resolved and nothing is dialled — this asks whether the string is
// an address, not whether anybody is at it.
//
// **The unspecified addresses are refused here and nowhere else, which is the one rule this
// side adds.** `0.0.0.0:7777` and `[::]:7777` are what a server bound to every interface would
// announce if its operator pointed this flag at -listen, they are perfectly well-formed
// host:port, and internal/registry would therefore write them down and serve them — a row in
// the list that every client dials and none can reach. The listening side is the only side
// that knows the difference between "bind everywhere" and "come to this address", so it is the
// side that has to refuse it.
//
// **The address is not quoted back into the refusal.** It locates somebody's house and this
// error reaches a log; the flag name and the rule are what an operator needs, and they carry no
// address.
func validateAnnounceAddress(addr string) error {
	if !printableASCIIKey(addr) {
		return errors.New("-announce-address is printable ASCII with no spaces, and this one is not")
	}

	host, port, err := net.SplitHostPort(addr)
	if err != nil {
		return errors.New("-announce-address is not host:port; a player dials a host and a port, so both are needed")
	}
	if host == "" {
		return errors.New("-announce-address names no host; a bare \":7777\" is what a listener binds, not somewhere a player can be sent")
	}
	if ip := net.ParseIP(host); ip != nil && ip.IsUnspecified() {
		return errors.New("-announce-address is the unspecified address, which means \"every interface\" to whoever binds " +
			"it and nothing at all to whoever dials it; announce the address players reach this server at")
	}

	number, err := strconv.Atoi(port)
	if err != nil {
		return errors.New("-announce-address needs a numeric port; a named one resolves against the /etc/services of " +
			"whichever machine reads it")
	}
	if number < 1 || number > 65535 {
		// Zero is excluded as well as the out-of-range values: :0 means "any free port" to
		// whoever binds it and means nothing at all to whoever dials it.
		return fmt.Errorf("the port in -announce-address must be in 1..65535, got %d", number)
	}
	return nil
}

// loop announces this server, then keeps announcing it, until ctx ends.
//
// saveStructuresLoop's shape and its answer to failure: a pass that fails is logged and the
// next one tries again, because an account service that is down is a reason to shout and not
// a reason for a game to stop. It returns ctx.Err() on cancellation, which is what puts it in
// the shutdown ordering beside the autosaves.
//
// **A nil announcer's loop returns at once**, which is the same shape openWorld and
// openPlayers use for an ephemeral world: the branch lives here rather than at the one call
// site, so a server nobody asked to announce costs one function call.
//
// The first announcement is immediate rather than one interval away — the listener is already
// up and the certificate is already known by the time this worker starts, so there is nothing
// left to wait for, and a restart that waited a minute would be a minute of players being
// sent to an address this server has left.
func (a *announcer) loop(ctx context.Context) error {
	if a == nil {
		return nil
	}

	for {
		// Checked before the announcement and again in the select below. `select` with two
		// ready cases picks at random, so an already-cancelled context is honoured about half
		// the time without this — which surfaces later as one more request during shutdown,
		// or as a flaky test.
		if err := ctx.Err(); err != nil {
			return err
		}
		a.announce(ctx)

		timer := time.NewTimer(a.every)
		select {
		case <-ctx.Done():
			timer.Stop()
			return ctx.Err()
		case <-timer.C:
		}
	}
}

// announce makes one attempt and reports nothing to its caller, because there is nothing a
// caller could do about it. Every outcome ends in a log line and another interval.
func (a *announcer) announce(ctx context.Context) {
	ctx, cancel := context.WithTimeout(ctx, a.timeout)
	defer cancel()

	ack, err := a.post(ctx)
	if err != nil {
		var failed *announceError
		if errors.As(err, &failed) {
			a.failed(failed.reason, failed.detail)
			return
		}
		// Unreachable: post returns nothing else. Classified rather than dropped, so a later
		// error path cannot become a silent pass.
		a.failed("unclassified", err.Error())
		return
	}
	a.succeeded(ack)
}

// announceError is a failed pass in the two pieces the log needs: a reason stable enough to
// compare against the previous pass, and a detail safe enough to print.
//
// The split exists because of the comparison. A service that has been unreachable for an hour
// must be one warning rather than sixty, and the thing to compare is *what went wrong* rather
// than the message, which carries changing text — a source port, an elapsed time.
type announceError struct {
	reason string
	detail string
}

func (e *announceError) Error() string { return e.reason + ": " + e.detail }

// post makes the request and reads the acknowledgement.
//
// **Nothing from a response body reaches this server's log except a code out of the closed set
// cmd/voxelheim-auth answers with**, and nothing from the transport error reaches it verbatim:
// net/http wraps every failure in a *url.Error whose message is the URL it was given, which is
// the one string an operator may have written a password into. Unwrapping it to the inner
// error is what keeps -account-service's userinfo inside this process, and it is why
// tickets.go's spelling — which wraps the *url.Error whole — is not copied here: that one runs
// once at startup, this one runs for the life of the server.
func (a *announcer) post(ctx context.Context) (registration, error) {
	body, err := json.Marshal(announcement{
		Name:              a.name,
		Address:           a.address,
		CertificateSHA256: a.fingerprint,
	})
	if err != nil {
		// Unreachable: three strings always marshal. Reported rather than ignored.
		return registration{}, &announceError{reason: "unencodable", detail: err.Error()}
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.endpoint.String(), bytes.NewReader(body))
	if err != nil {
		return registration{}, &announceError{reason: "unrequestable", detail: stripURL(err).Error()}
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	// **The one place the key leaves its type**, which is what the named Reveal buys: every
	// other route out of a registrationKey is a redaction, and this line is the grep.
	req.Header.Set("Authorization", "Bearer "+a.key.Reveal())

	resp, err := a.client.Do(req)
	if err != nil {
		return registration{}, &announceError{reason: "unreachable", detail: stripURL(err).Error()}
	}
	defer func() { _ = resp.Body.Close() }()

	// One byte more than the bound is read, so that a body *at* the limit is told apart from
	// one that was cut off at it. Read before the status is judged, because a refusal carries
	// the code that says which field was wrong.
	raw, readErr := io.ReadAll(io.LimitReader(resp.Body, maxAnnounceResponseBytes+1))
	switch {
	case readErr != nil:
		return registration{}, &announceError{
			reason: "unreadable",
			detail: fmt.Sprintf("the service answered %s and the answer could not be read: %s",
				resp.Status, stripURL(readErr).Error()),
		}
	case len(raw) > maxAnnounceResponseBytes:
		return registration{}, &announceError{
			reason: "oversized",
			detail: fmt.Sprintf("the service answered %s with more than the %d bytes a registration answer can be",
				resp.Status, maxAnnounceResponseBytes),
		}
	}

	if resp.StatusCode != http.StatusOK {
		return registration{}, &announceError{
			reason: "status " + strconv.Itoa(resp.StatusCode),
			detail: refusedDetail(resp.Status, raw),
		}
	}

	var ack registration
	if err := json.Unmarshal(raw, &ack); err != nil {
		// The decode error is not printed: encoding/json quotes bytes of the offending input
		// in some of its messages, and those bytes came from whoever answered that address.
		return registration{}, &announceError{
			reason: "unreadable",
			detail: "the service answered 200 with something that is not the JSON this endpoint publishes",
		}
	}
	return ack, nil
}

// refusedDetail is what a non-200 is written down as: the status always, and the refusal code
// only when it is one this service's own vocabulary contains.
//
// The code is the genuinely useful half — `address_refused` and `unauthorized` send an
// operator to two entirely different lines of their configuration — and the closed set is what
// makes printing it safe. Whatever answered that address is not known to be the account
// service, so its free text is a stranger writing in this server's log.
func refusedDetail(status string, body []byte) string {
	var refused refusal
	if err := json.Unmarshal(body, &refused); err == nil && knownRefusals[refused.Error] {
		return "the service refused the registration with " + status + ": " + refused.Error
	}
	return "the service answered " + status
}

// stripURL is err with net/http's *url.Error wrapper removed.
//
// url.Error renders as `Post "http://user:pass@host/v1/servers": …`, so its message carries
// whatever userinfo an operator wrote into -account-service. The inner error is the part worth
// reading — "connection refused", "context deadline exceeded" — and it carries no URL. An
// error that is not a *url.Error is returned unchanged.
func stripURL(err error) error {
	var wrapped *url.Error
	if errors.As(err, &wrapped) && wrapped.Err != nil {
		return wrapped.Err
	}
	return err
}

// failed records a pass that did not land, and decides how loudly to say so.
//
// **The first failure and every change of reason is a warning; an identical repeat is a debug
// line.** Every failed announcement is logged either way — that is the acceptance criterion —
// but an account service that has been down since lunchtime is one thing that is wrong, not
// three hundred, and a warning that repeats on a timer is a warning nobody reads. A change of
// reason is worth a second warning because it is a second thing: `unreachable` becoming
// `status 401` is the service coming back up and refusing the key.
func (a *announcer) failed(reason, detail string) {
	// lastFailure is cleared on every success and no reason is ever empty, so this one
	// comparison answers both halves: the same thing is still wrong, and it was still wrong
	// last time.
	repeat := reason == a.lastFailure
	a.announced = false
	a.lastFailure = reason

	if repeat {
		a.log.Debug("announcing this server failed again; the next pass will try again",
			"endpoint", a.redacted, "world_name", a.name, "reason", reason, "detail", detail)
		return
	}
	// **Never fatal, and the line says so**, because the operator reading it is deciding
	// whether their game is broken. It is not: players already connected stay connected, and
	// players who know the address can still join. What is lost is the list being current.
	a.log.Warn("announcing this server failed; it keeps serving and the next pass will try again",
		"endpoint", a.redacted, "world_name", a.name, "reason", reason, "detail", detail)
}

// succeeded records a pass that landed, and takes the interval the account service published.
func (a *announcer) succeeded(ack registration) {
	// The first success, and every one that follows a failure, is worth a line; the ninety
	// after that are a metronome. Debug is where the metronome belongs.
	news := !a.announced || a.lastFailure != ""
	a.announced = true
	a.lastFailure = ""

	every := a.settle(ack.OfflineAfterSeconds)
	if news {
		// The address is absent here for the reason it is absent from the startup line, and
		// **the name is this server's own rather than the one the acknowledgement echoes**:
		// a response body is a third party's text, and the only field of one this server
		// ever writes down is a refusal code out of the closed set above. What is read from
		// the body is the two values that are not text — whether the registration was new,
		// and the window it published.
		a.log.Info("this server is in the account service's list",
			"endpoint", a.redacted,
			"world_name", a.name,
			"created", ack.Created,
			"certificate_sha256", a.fingerprint,
			"every", every.String())
		return
	}
	a.log.Debug("this server announced itself",
		"endpoint", a.redacted, "world_name", a.name, "every", every.String())
}

// settle narrows the announce interval to what the account service says the window is, and
// answers what it settled on.
//
// **This is how `registry.OfflineAfter` is read rather than copied.** That constant is
// documented as the number the announcing side must be under, and this process may not import
// the package that holds it, so the account service publishes it in every acknowledgement and
// this is where it is believed — within limits, because "answering nonsense" is one of the
// three ways this call is expected to go wrong.
//
// The rule in one line: never slower than [announceTriesPerWindow] announcements inside the
// published window, and never faster than the interval this announcer was configured with.
//
//   - It only ever *narrows*. A service publishing a huge window cannot slow this down, and a
//     test that shortened the interval keeps the interval it shortened.
//   - The derived value has a floor. A service publishing four seconds would otherwise derive
//     one, and a service answering nonsense would become a hot loop against somebody's machine.
//   - A number outside 1..[maxOfflineAfterSeconds] is ignored entirely, which covers the
//     absent field, a zero, a negative and an overflow.
func (a *announcer) settle(offlineAfterSeconds int64) time.Duration {
	if offlineAfterSeconds < 1 || offlineAfterSeconds > maxOfflineAfterSeconds {
		return a.every
	}

	window := time.Duration(offlineAfterSeconds) * time.Second
	derived := max(window/announceTriesPerWindow, minAnnounceEvery)
	a.every = min(a.every, derived)
	return a.every
}
