package main

import (
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// The account service's route that publishes the verifying half of its signing key,
// and the shape of what it answers.
//
// Both are stated here rather than shared with cmd/voxelheim-auth, and the split is
// deliberate: that command is the account service, this one is a game server, and they
// are two programs that meet over HTTP. A shared struct would be an import from one
// service into the other, which is exactly the coupling `internal/ticket` is a leaf in
// order to avoid — and it would hide a wire break behind a compile that still passes.
// What keeps the two in step is the field names below and `ticket.Algorithm`, which is
// the value both sides read from the one package that owns it.
const ticketKeyPath = "/v1/ticket-key"

// ticketKeyResponse is the part of that answer this server reads.
//
// The lifetime the endpoint also publishes is deliberately not here: a ticket's expiry
// is inside the ticket and signed, so nothing this server decides may depend on a number
// the endpoint states in prose beside it.
type ticketKeyResponse struct {
	Algorithm string `json:"algorithm"`
	PublicKey string `json:"public_key"`
}

// fetchTicketKeyTimeout bounds the one request this server ever makes to the account
// service.
//
// Generous, because it happens once and the alternative to waiting is refusing to
// start; bounded, because an operator watching a server come up should be told that the
// address is wrong rather than left looking at a process that never says anything. The
// context this runs under is the signal-cancelled one, so ctrl-C still works during it.
const fetchTicketKeyTimeout = 10 * time.Second

// maxTicketKeyResponseBytes bounds what is read from that answer before anything is
// parsed.
//
// The ordering is the point, and it is the one `ticket.Decode` and `registry.ParseKey`
// both make one protocol up: the response is as long as whoever answered chose, and a
// key is 64 characters of hex inside a small JSON object. Four kilobytes is orders of
// magnitude more than that and still nothing a machine notices, so a body that exceeds
// it is refused rather than buffered.
const maxTicketKeyResponseBytes = 4096

// validateTicketKeySource enforces the one rule about where the key comes from that can
// be checked without asking anybody: exactly one source, named.
//
// **Mutually exclusive rather than ordered**, which is `internal/registry`'s rule for its
// two key sources and it is here for the same reason: a precedence rule is something an
// operator has to remember, and one who has set both has already made a mistake worth
// being told about.
//
// **And neither is a refusal to start.** That is the acceptance criterion and it is worth
// saying why it is not a warning: the alternative to a key is a server that admits people
// it cannot check, which is the second way in this whole design exists to remove. A server
// that cannot verify a ticket should be visibly broken rather than quietly open.
func (o options) validateTicketKeySource() error {
	switch {
	case o.accountService != "" && o.ticketKey != "":
		return errors.New("-account-service and -ticket-key are mutually exclusive: give the address to read the " +
			"key from, or the key itself, and not both")
	case o.ticketKey != "":
		_, err := parseTicketKey(o.ticketKey)
		return err
	case o.accountService != "":
		_, err := parseAccountService(o.accountService)
		return err
	default:
		return errors.New("this server has no ticket key: give it -account-service to read one from, or " +
			"-ticket-key to use one that was copied by hand. A server that cannot verify a session ticket " +
			"cannot admit anybody, so it refuses to start rather than starting with no doorman")
	}
}

// openVerifier settles who this server will admit, once, before anything else.
//
// **The last thing that touches the network on the admission path.** From here on
// admitting a player is arithmetic over bytes this process already holds: no call to the
// account service, no lookup, nothing that can be slow or down. That is the whole design
// — see `internal/ticket`'s package doc — and the shape of this function is what makes it
// true, because a key fetched per join would have rebuilt the hard dependency the ticket
// exists to remove.
func openVerifier(ctx context.Context, opts options, log *slog.Logger) (*session.Verifier, error) {
	world, err := ticket.WorldIDFor(opts.worldName)
	if err != nil {
		// Unreachable: options.validate has already asked the same function. Kept
		// because this function is the one that must not build a verifier out of a
		// world nobody checked, and a second call to a pure function costs nothing.
		return nil, fmt.Errorf("invalid -world-name: %w", err)
	}

	key, source, err := ticketKey(ctx, opts, log)
	if err != nil {
		return nil, err
	}

	verifier, err := session.NewVerifier(key, world, time.Now)
	if err != nil {
		return nil, err
	}

	// The public key is logged on every start, deliberately, exactly as the account
	// service logs it: it is public, and it is the one number an operator can compare
	// against what that service says to settle "are these two talking about the same
	// key". The world id is a digest of a name an operator publishes, so it is safe
	// beside it, and printing both is what makes a fleet-wide misconfiguration legible
	// in one line rather than in a refusal per player.
	log.Info("session tickets will be verified offline",
		"world_name", opts.worldName,
		"world_id", world.String(),
		"ticket_algorithm", ticket.Algorithm,
		"ticket_key", hex.EncodeToString(key),
		"ticket_key_source", source)
	return verifier, nil
}

// ticketKey answers the account service's public key and where it came from.
func ticketKey(ctx context.Context, opts options, log *slog.Logger) (ed25519.PublicKey, string, error) {
	if err := opts.validateTicketKeySource(); err != nil {
		return nil, "", err
	}
	if opts.ticketKey != "" {
		key, err := parseTicketKey(opts.ticketKey)
		return key, "-ticket-key", err
	}

	base, err := parseAccountService(opts.accountService)
	if err != nil {
		return nil, "", err
	}
	key, err := fetchTicketKey(ctx, base, log)
	return key, base.JoinPath(ticketKeyPath).String(), err
}

// parseTicketKey reads a public key an operator or an endpoint stated in hex.
//
// Case is not part of the value here, unlike a certificate fingerprint in
// `internal/registry`, which is refused rather than folded. The difference is what
// happens to the string: a fingerprint is *compared as text*, so two spellings are two
// values that eventually fail to match; this one is decoded to bytes and compared as
// bytes, so a capital letter cannot silently mean a different key.
func parseTicketKey(encoded string) (ed25519.PublicKey, error) {
	raw, err := hex.DecodeString(strings.TrimSpace(encoded))
	if err != nil {
		// The input is not echoed. It is not a secret — this is the public half — but
		// an error message reaches a log, and quoting an operator's whole flag value
		// into one buys nothing a length does not.
		return nil, fmt.Errorf("the ticket key is not hex: %w", err)
	}
	if len(raw) != ed25519.PublicKeySize {
		return nil, fmt.Errorf("%w, got %d", ticket.ErrPublicKeySize, len(raw))
	}
	return raw, nil
}

// parseAccountService reads the base URL the key is fetched from, refusing the shapes
// that cannot mean anything.
//
// A scheme and a host are required because `http.Client` will otherwise fail at the
// request with a message about a URL rather than about a flag, and a path this server
// appends to is required to be a path — a query or a fragment on a base is somebody
// having pasted the wrong thing.
func parseAccountService(raw string) (*url.URL, error) {
	base, err := url.Parse(strings.TrimSpace(raw))
	if err != nil {
		return nil, fmt.Errorf("invalid -account-service: %w", err)
	}
	switch {
	case base.Scheme != "http" && base.Scheme != "https":
		return nil, fmt.Errorf("invalid -account-service: the scheme must be http or https, got %q", base.Scheme)
	case base.Host == "":
		return nil, errors.New("invalid -account-service: it names no host")
	case base.RawQuery != "" || base.Fragment != "":
		return nil, errors.New("invalid -account-service: it is a base address, so it carries no query and no fragment")
	}
	return base, nil
}

// fetchTicketKey reads the key from the account service, once.
//
// **What this call cannot tell you is that it reached the right service, and that is a
// known gap rather than an oversight** (#131). The endpoint is deliberately
// unauthenticated — a public key is public — so the exposure is not confidentiality but
// substitution: anybody able to answer for that address hands this server their own
// public key, and this server then admits tickets they minted for any account and
// refuses every real one. Because the documented pattern is to read the key once and
// keep it, that substitution outlives whoever performed it. Nothing here closes that,
// and nothing here pretends to; the warning below is what an operator gets today, and
// -ticket-key is the way to avoid the fetch entirely until #131 lands.
func fetchTicketKey(ctx context.Context, base *url.URL, log *slog.Logger) (ed25519.PublicKey, error) {
	if base.Scheme != "https" {
		log.Warn("the account service's key is being read over an unauthenticated connection; anybody able to "+
			"answer for that address can hand this server a key of their own, and this server would then admit "+
			"the tickets they mint and refuse every real one",
			"account_service", base.Redacted())
	}

	ctx, cancel := context.WithTimeout(ctx, fetchTicketKeyTimeout)
	defer cancel()

	endpoint := base.JoinPath(ticketKeyPath).String()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if err != nil {
		return nil, fmt.Errorf("reading the ticket key from %s: %w", endpoint, err)
	}
	req.Header.Set("Accept", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("reading the ticket key from %s: %w", endpoint, err)
	}
	defer func() {
		// Drained before closing so the connection can be reused — a habit rather than
		// a need for a request made once — and both results discarded, because a
		// failure to tidy up after a response already read is not a reason to refuse to
		// start.
		_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, maxTicketKeyResponseBytes))
		_ = resp.Body.Close()
	}()

	if resp.StatusCode != http.StatusOK {
		// The status and nothing from the body: whatever answered is not known to be
		// the account service, so its bytes are not something to put in this server's
		// log.
		return nil, fmt.Errorf("reading the ticket key from %s: the service answered %s", endpoint, resp.Status)
	}

	// One byte more than the bound is read, so that a body *at* the limit is told apart
	// from one that was cut off at it.
	body, err := io.ReadAll(io.LimitReader(resp.Body, maxTicketKeyResponseBytes+1))
	if err != nil {
		return nil, fmt.Errorf("reading the ticket key from %s: %w", endpoint, err)
	}
	if len(body) > maxTicketKeyResponseBytes {
		return nil, fmt.Errorf("reading the ticket key from %s: the answer is longer than the %d bytes a key "+
			"response can be", endpoint, maxTicketKeyResponseBytes)
	}

	var published ticketKeyResponse
	if err := json.Unmarshal(body, &published); err != nil {
		return nil, fmt.Errorf("reading the ticket key from %s: the answer is not the JSON this endpoint "+
			"publishes: %w", endpoint, err)
	}
	if published.Algorithm != ticket.Algorithm {
		// Compared rather than assumed, which is the reason the endpoint publishes it:
		// a key of the right length under a different scheme is bytes this server would
		// otherwise have verified Ed25519 signatures with.
		return nil, fmt.Errorf("reading the ticket key from %s: the key is for %q and this server verifies %q",
			endpoint, published.Algorithm, ticket.Algorithm)
	}

	key, err := parseTicketKey(published.PublicKey)
	if err != nil {
		return nil, fmt.Errorf("reading the ticket key from %s: %w", endpoint, err)
	}
	return key, nil
}
