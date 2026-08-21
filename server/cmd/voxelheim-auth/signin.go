package main

import (
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"strings"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/auth"
	"github.com/FabioSM46/voxelheim-v2/server/internal/discord"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// maxSignInRequestBytes bounds the finish request's body.
//
// A state is 43 characters and a Discord authorization code is around thirty; four
// kilobytes is far more than either needs and is what stops an unauthenticated POST
// from being a way to hand this process an arbitrary amount of JSON to decode.
const maxSignInRequestBytes = 4 << 10

// defaultDiscordRedirectURI is where Discord sends the browser when nobody has said
// otherwise: a loopback address on the *player's* machine, caught by a listener the
// client opens for the length of one sign-in. Nothing in this service ever binds it.
const defaultDiscordRedirectURI = "http://127.0.0.1:7780/discord/callback"

// signIn is the sign-in half of the service: the provider flow, and the store that
// turns the identity it produces into an account.
//
// The two are joined here and nowhere else. internal/discord never learns that an
// account exists and internal/auth never learns that Discord does, which is what keeps
// the provider flow testable against an httptest.Server and the account store testable
// against a directory.
type signIn struct {
	flow     *discord.Flow
	accounts *auth.Store
}

// newSignIn builds the sign-in flow, or reports that this deployment has not been
// given one.
//
// **A nil flow and a nil error is "not configured", and it is a deliberate state
// rather than an oversight.** A Discord application is something an operator registers
// and this service cannot invent; refusing to start without one would mean the account
// service could not run at all until somebody had — including in every test that is
// about the store, the port or the health probe. So the routes exist either way and
// answer 503 until a client id is given, which is a service that says what is missing
// rather than one that is silently absent.
//
// The rest of the checking is discord.New's rather than options.validate's, and
// deliberately: a redirect URI is not a value that gets narrowed on its way to being
// used, so the rule that puts -listen's range check in validate does not reach it — and
// restating discord.New's refusals there would be a second implementation of them. The
// ordering property validate buys is kept instead by calling this before the listener is
// bound, which TestASignInThatCannotBeConfiguredRefusesBeforeThePortIsBound pins.
func newSignIn(opts options, accounts *auth.Store, log *slog.Logger) (*signIn, error) {
	if strings.TrimSpace(opts.discordClientID) == "" {
		log.Warn("Discord sign-in is not configured; its routes will refuse every request",
			"flag", "-discord-client-id")
		return nil, nil
	}

	flow, err := discord.New(discord.Config{
		ClientID:    opts.discordClientID,
		RedirectURI: opts.discordRedirectURI,
	})
	if err != nil {
		return nil, err
	}
	log.Info("Discord sign-in configured", "provider", discord.Provider, "redirect_uri", opts.discordRedirectURI)
	return &signIn{flow: flow, accounts: accounts}, nil
}

// The error vocabulary this service answers refusals with, and the status each carries.
//
// **A code in the body rather than a status alone**, because a status collapses
// distinctions the client needs to act on: three of these share the 400, two share the
// 503 and two share the 500, and "start again, that sign-in has expired" is a different
// thing for a player to be told than "the provider is down, wait" or "this deployment
// was never given a Discord application". The codes are a closed set defined here, so a
// refusal never carries a word that came from the provider or from the request.
//
// Every 4xx here is a 400 and none is a 401. A 401 without a WWW-Authenticate header is
// not what that status means, and inventing an authentication scheme to satisfy it
// would be inventing a scheme — the request is simply not one this service can act on,
// which is what 400 says.
const (
	errNotConfigured       = "sign_in_not_configured"
	errMalformedRequest    = "malformed_request"
	errSignInNotFound      = "sign_in_not_found"
	errProviderRefused     = "provider_refused"
	errProviderUnavailable = "provider_unavailable"
	errTooManySignIns      = "too_many_sign_ins"
	errAccountUnavailable  = "account_unavailable"
	errSignInCouldNotStart = "sign_in_could_not_start"

	// A sign-in that names no world this service can issue a ticket for. Its own code
	// rather than errMalformedRequest, for the reason the whole vocabulary exists: the
	// two are different things for a client to be told. "Your JSON was unreadable" sends
	// somebody looking at their encoder; "the world you named is not one I will sign
	// for" sends them to look at their configuration, which is where the mistake is.
	errWorldNotNamed = "world_not_named"

	// A ticket that could not be minted. Reachable only through internal/ticket refusing
	// to sign — a clock far outside the range the format holds, or an account id this
	// service should not have produced — so it is a 500 and not a 400: nothing about the
	// request caused it.
	errTicketUnavailable = "ticket_unavailable"
)

// startResponse is what a client needs to open a browser and come back.
//
// **`finish_secret` is the field that must never be put in a URL.** It is what proves
// the caller finishing a sign-in is the one that started it, and it works only for as
// long as it stays out of the browser: the redirect the provider sends back carries
// `code` and `state` together, so anything that can read that URL already holds both.
// A client keeps this in memory and presents it to `finish`. See
// [discord.Start.FinishSecret].
type startResponse struct {
	State        string    `json:"state"`
	FinishSecret string    `json:"finish_secret"`
	AuthorizeURL string    `json:"authorize_url"`
	ExpiresAt    time.Time `json:"expires_at"`
}

// finishRequest is the state and the code the client caught on its loopback listener.
//
// Both are [discord.Secret], which is what puts them inside the type that redacts
// itself from the moment this service holds them: a decoded request body is the very
// first place either value exists here, so any later slip has to escape the type
// deliberately.
type finishRequest struct {
	State discord.Secret `json:"state"`
	Code  discord.Secret `json:"code"`
	// FinishSecret is the one field here the loopback listener did not catch: the
	// client held it from `start`. Without it, `state` was a bearer credential.
	FinishSecret discord.Secret `json:"finish_secret"`

	// World is the world this sign-in wants a ticket for, and it is required.
	//
	// **A ticket is minted here because there is nowhere else it could be.** A separate
	// endpoint would need the caller to prove who they are, and the only thing that ever
	// proved that is the authorization code this request is spending — a credential that
	// outlived the sign-in so a ticket could be asked for later is exactly the refresh
	// token this design does not have. So the world has to be named up front, and a
	// player who wants to join a second world signs in again.
	//
	// Not a [discord.Secret]: a world name is an identifier an operator publishes to
	// everybody who might play there, and treating it as a secret would only make it
	// harder to put in the log line that says which world a sign-in was for.
	World string `json:"world"`
}

// finishResponse is who the client now is, and the credential it plays with.
//
// **`session_ticket` is a bearer credential and the last one this flow hands out.**
// Whatever holds those bytes is that account on that world until they expire — a
// signature proves who issued a ticket, not who is presenting it — so the schema's rule
// applies to it here exactly as it does on the wire: never logged, never displayed. It
// leaves [ticket.Ticket] through the one named method that exists for the purpose, which
// is what keeps every other route out of it.
type finishResponse struct {
	AccountID   string `json:"account_id"`
	DisplayName string `json:"display_name"`
	Created     bool   `json:"created"`

	// SessionTicket is the ticket, unpadded base64url, to be presented verbatim as
	// `ClientHello.session_ticket` after the client has decoded it.
	SessionTicket string `json:"session_ticket"`

	// TicketExpiresAt is when it stops working, so a client can say so rather than
	// discovering it at a handshake. Read from what was actually signed rather than
	// computed a second time from the lifetime.
	//
	// **There is no revocation, so this is the only way a ticket ever ends.** A client
	// that has lost one cannot cancel it; it can only sign in again for a new one and
	// wait for the old one to run out.
	TicketExpiresAt time.Time `json:"ticket_expires_at"`
}

// signInStart begins a sign-in and answers with where to send the browser.
//
// POST rather than GET: it mints a state and a PKCE verifier and files them, so it
// changes this service's state and must not be something a link or a prefetch can do.
func (s *service) signInStart(w http.ResponseWriter, _ *http.Request) {
	if s.signin == nil {
		s.refuse(w, http.StatusServiceUnavailable, errNotConfigured)
		return
	}

	start, err := s.signin.flow.Begin()
	if err != nil {
		if errors.Is(err, discord.ErrTooManyPending) {
			// Info rather than warn: a busy service refusing the newest of thousands of
			// sign-ins is working as designed, and this is the line that says so.
			s.log.Info("a sign-in was refused because too many are already in flight")
			s.refuse(w, http.StatusServiceUnavailable, errTooManySignIns)
			return
		}
		// The only other way Begin fails is a failed read from crypto/rand, which
		// carries nothing about anybody.
		s.log.Error("a sign-in could not be started", "error", err)
		s.refuse(w, http.StatusInternalServerError, errSignInCouldNotStart)
		return
	}

	s.writeJSON(w, http.StatusOK, startResponse{
		// Revealed exactly once, here, because both have to reach the client or the flow
		// cannot work. Everywhere else they stay inside discord.Secret.
		State:        start.State.Reveal(),
		FinishSecret: start.FinishSecret.Reveal(),
		AuthorizeURL: start.AuthorizeURL,
		ExpiresAt:    start.ExpiresAt,
	})
}

// signInFinish redeems an authorization code and answers with the account behind it and
// a ticket it can join with.
//
// The order is the whole of the rule this endpoint exists to keep: the world is checked
// first, the provider is asked second and the account store third, so an account is
// created only after somebody has actually proved who they are. There is no path here on
// which a refusal leaves an account behind.
//
// **The world is checked before the provider is called, and that ordering is not
// tidiness.** An authorization code may be redeemed once; refusing after the redemption
// would spend somebody's code — and mint them an account — for a mistake this service
// could see in the request body without asking anybody anything, leaving them to start
// the whole sign-in again.
func (s *service) signInFinish(w http.ResponseWriter, r *http.Request) {
	if s.signin == nil {
		s.refuse(w, http.StatusServiceUnavailable, errNotConfigured)
		return
	}

	var req finishRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxSignInRequestBytes)).Decode(&req); err != nil {
		// **The decode error is not logged**, and that is not tidiness. It is built from
		// the request body, and this request body carries an authorization code — a
		// diagnostic derived from a credential is a diagnostic that can carry one, and
		// encoding/json does quote bytes of the offending input in some of its messages.
		// The refusal code the client is given already says what went wrong.
		s.log.Info("a sign-in request could not be read")
		s.refuse(w, http.StatusBadRequest, errMalformedRequest)
		return
	}

	world, err := ticket.WorldIDFor(req.World)
	if err != nil {
		// **The name is not quoted back into the log**, and internal/ticket does not
		// quote it into the error either. It is text from an unauthenticated request
		// body, and a log line that echoes one is a log line an attacker writes.
		s.log.Info("a sign-in was refused", "reason", "the request does not name a world a ticket can be issued for")
		s.refuse(w, http.StatusBadRequest, errWorldNotNamed)
		return
	}

	who, err := s.signin.flow.Redeem(r.Context(), req.State, req.Code, req.FinishSecret)
	switch {
	case errors.Is(err, discord.ErrMalformedRequest):
		// A field is missing, which is not the same as a sign-in that cannot be found:
		// nothing was looked up, so answering `sign_in_not_found` would state something
		// this service does not know. The two were one answer until #122's review.
		s.log.Info("a sign-in was refused", "reason", "the redemption is missing a field")
		s.refuse(w, http.StatusBadRequest, errMalformedRequest)
		return
	case errors.Is(err, discord.ErrNoSuchSignIn):
		s.log.Info("a sign-in was refused", "reason", "no sign-in is waiting for that state")
		s.refuse(w, http.StatusBadRequest, errSignInNotFound)
		return
	case errors.Is(err, discord.ErrRejected):
		s.log.Info("a sign-in was refused", "reason", "the provider refused the authorization code")
		s.refuse(w, http.StatusBadRequest, errProviderRefused)
		return
	case err != nil:
		// Unreachable, slow, erroring, or answering something unreadable. Warn, because
		// this one is about the provider or the network rather than about the request,
		// and an operator seeing a run of them is seeing an outage.
		s.log.Warn("a sign-in could not reach the provider", "error", err)
		s.refuse(w, http.StatusBadGateway, errProviderUnavailable)
		return
	}

	account, created, err := s.signin.accounts.Ensure(
		auth.ProviderIdentity{Provider: discord.Provider, Subject: who.Subject},
		who.DisplayName,
		time.Now(),
	)
	switch {
	case errors.Is(err, auth.ErrInvalidIdentity):
		// The provider answered with something this service cannot key an account on.
		// That is the provider's failure and not the request's, so it is reported as
		// one — and no account was minted, which is the property that matters.
		s.log.Warn("the provider named an identity this service cannot key on", "error", err)
		s.refuse(w, http.StatusBadGateway, errProviderUnavailable)
		return
	case err != nil:
		// An unreadable record stops here rather than becoming a second account for
		// somebody who already has one; auth.Store.Ensure is where that refusal lives
		// and this is what it looks like from outside.
		s.log.Error("an account could not be resolved", "error", err)
		s.refuse(w, http.StatusInternalServerError, errAccountUnavailable)
		return
	}

	// The conversion is the seam between the two packages, and it is the only thing
	// holding their two sixteen-byte ids together: internal/ticket keeps its own so that
	// the game server can import it without reaching the accounts, and this line stops
	// compiling if either width ever moves.
	sessionTicket, claims, err := s.keys.Mint(ticket.AccountID(account.ID), world, time.Now())
	if err != nil {
		// The account already exists at this point, and that is not a failure to undo:
		// an account is idempotent, so the player retries and gets the same one with a
		// ticket. Reachable only through internal/ticket refusing to sign, which is this
		// service's own mistake and not the request's — hence a 500.
		s.log.Error("a session ticket could not be minted", "error", err, "account_id", account.ID.String())
		s.refuse(w, http.StatusInternalServerError, errTicketUnavailable)
		return
	}

	// The account id is safe to log by construction: it is minted at random and derived
	// from nothing about the person, so it names them here without describing them. The
	// display name is deliberately absent — it is personal data, and nothing needs it in
	// a log. The world id is the digest of a name this service already validated, so it
	// is neither personal nor attacker-chosen text; the ticket itself is a bearer
	// credential and is not here at all.
	s.log.Info("sign-in completed",
		"provider", discord.Provider,
		"account_id", account.ID.String(),
		"created", created,
		"world_id", claims.World.String(),
		"ticket_expires_at", claims.ExpiresAt)

	s.writeJSON(w, http.StatusOK, finishResponse{
		AccountID:   account.ID.String(),
		DisplayName: account.DisplayName,
		Created:     created,
		// Encoded exactly once, here, because the ticket has to reach the client or
		// there was no point minting it. Everywhere else it stays inside ticket.Ticket,
		// which redacts itself through fmt, %#v, log/slog and encoding/json alike.
		SessionTicket:   sessionTicket.Encode(),
		TicketExpiresAt: claims.ExpiresAt,
	})
}

// errorResponse is the shape every refusal takes.
type errorResponse struct {
	Error string `json:"error"`
}

// refuse answers with one of the codes above and nothing else.
func (s *service) refuse(w http.ResponseWriter, status int, code string) {
	s.writeJSON(w, status, errorResponse{Error: code})
}

// writeJSON writes one JSON value, and treats a failed write as a client that hung up.
//
// The status goes out before the body, so a marshal that fails cannot change it. That
// is why body is a type declared in this file rather than anything a caller assembles:
// every one of them marshals, so the failure below is a broken pipe and not a bug.
func (s *service) writeJSON(w http.ResponseWriter, status int, body any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)

	if err := json.NewEncoder(w).Encode(body); err != nil {
		// Debug, and never an error: a client that hangs up before reading the response
		// is a client-side event, and the whole of what it costs is this response.
		s.log.Debug("a sign-in response could not be written", "error", err)
	}
}
