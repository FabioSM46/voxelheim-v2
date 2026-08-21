// Command voxelheim-auth is Voxelheim's account service.
//
// It keeps who the people playing here are — one account per provider identity, under
// its own directory — and answers over HTTP. It shares a Go module with the game
// server and nothing else: not a port, not a directory, not a package beyond the
// standard library and internal/auth. cmd/voxelheimd does not import that package and
// must not; the two are separate trust domains that happen to ship together, and
// internal/auth's imports_test.go is what says so.
//
// It signs people in with the Discord account they already have — OAuth 2.0
// Authorization Code with PKCE, as a public client, so there is no client secret here
// or in anything shipped to a player. internal/discord runs that flow and internal/auth
// records what it produces; signin.go is the one file that joins them.
//
// A finished sign-in hands back a **session ticket**: a short-lived, signed statement
// that this account may play on one named world, which the game server checks against a
// public key it read from this service once and kept. That is the whole point of the
// shape — the game server verifies a signature instead of asking permission, so this
// service being unreachable does not stop a game running on a machine that is perfectly
// fine. internal/ticket holds the key pair and mints; tickets.go publishes the public
// half.
//
// It also keeps the **server registry**: the list of game servers an operator has
// registered, each with the address players reach it at and the SHA-256 of the certificate
// it presents. That list is where the client's trust chain ends — the client knows this
// service by construction, this service knows the game servers because an operator
// registered them, and so a client can verify a server it has never met. Registration is
// authenticated with an operator-configured key and the list is readable only by an account
// holding a ticket; internal/registry holds the store and servers.go the two endpoints.
//
// What this command deliberately still does not do: withdraw a ticket before it
// expires. There is no revocation and none is planned — ticket.Lifetime is the whole of
// the answer, and the reasoning is written down beside the number.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"math"
	"net"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/auth"
	"github.com/FabioSM46/voxelheim-v2/server/internal/registry"
	"github.com/FabioSM46/voxelheim-v2/server/internal/ticket"
)

// The HTTP server's own deadlines, and the reason they are constants rather than
// flags: none of them is a policy an operator would want to differ between two
// deployments of this service, and a flag for every timeout is four more values that
// can be typed wrong. -listen is what the acceptance criteria call configurable, and
// it is what is configurable.
//
// ReadHeaderTimeout is the one that is not merely tidy. Without it a connection that
// opens and then dribbles one header byte a minute holds a goroutine for as long as
// it likes, and enough of them hold all of them — the whole of a Slowloris. Go's
// http.Server has no default for it, so the absence would be the setting.
const (
	readHeaderTimeout = 5 * time.Second
	readTimeout       = 15 * time.Second
	writeTimeout      = 15 * time.Second
	idleTimeout       = 60 * time.Second

	// shutdownTimeout bounds the graceful stop: in-flight requests finish, and a
	// request that will not finish does not hold the process open forever.
	shutdownTimeout = 10 * time.Second
)

func main() {
	opts := parseFlags()

	log, err := newLogger(opts.logLevel, opts.logFormat)
	if err != nil {
		fmt.Fprintf(os.Stderr, "voxelheim-auth: %v\n", err)
		os.Exit(2)
	}

	// NotifyContext turns the first SIGINT/SIGTERM into a cancelled context and leaves
	// a second one lethal — a shutdown that hangs must still be killable.
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	if err := run(ctx, opts, log); err != nil {
		log.Error("the account service stopped with an error", "error", err)
		os.Exit(1)
	}
}

type options struct {
	listen             string
	authDir            string
	discordClientID    string
	discordRedirectURI string

	// registrationKeyFile names a file holding the server-registration key, and is empty
	// when the key comes from the environment or when there is none. **The key itself is
	// never a flag** — see loadRegistrationKey, and note that this field holds a path
	// rather than a secret, which is exactly the point.
	registrationKeyFile string

	logLevel  string
	logFormat string
}

func parseFlags() options {
	var opts options

	// 7778 sits beside the game server's 7777 so that the pair is obvious in a
	// process list, and both default to the loopback interface: a service that binds
	// every interface by default is one somebody publishes by accident.
	flag.StringVar(&opts.listen, "listen", "127.0.0.1:7778", "address to listen on, as host:port; a :0 port binds a free one")
	flag.StringVar(&opts.authDir, "auth-dir", auth.DefaultAuthDir,
		"directory the accounts are stored in; unlike the game server's -world-dir there is no empty, "+
			"ephemeral form of this, because an account nobody kept is a person who cannot get back in")
	// A public client's id, which is public: PKCE is what stands in for a secret, and
	// there is no flag for one because there is no secret to give. Left empty, the
	// sign-in routes answer 503 rather than the service refusing to start — see
	// newSignIn.
	flag.StringVar(&opts.discordClientID, "discord-client-id", "",
		"the Discord application's client id; empty leaves Discord sign-in unconfigured and its routes refusing")
	flag.StringVar(&opts.discordRedirectURI, "discord-redirect-uri", defaultDiscordRedirectURI,
		"where Discord sends the browser back to; a loopback address on the player's machine, not on this service")
	// A path, never the key. A flag holding a credential is visible in `ps` to every user
	// on the machine and lands in shell history; the file it names should be readable only
	// by the user this service runs as. Leaving both this and VOXELHEIM_REGISTRATION_KEY
	// unset leaves registration unconfigured and its route refusing — see
	// loadRegistrationKey.
	flag.StringVar(&opts.registrationKeyFile, "registration-key-file", "",
		"file holding the key game servers register with; the key may be given in "+
			registrationKeyEnv+" instead, but not in both")
	flag.StringVar(&opts.logLevel, "log-level", "info", "log level: debug, info, warn or error")
	flag.StringVar(&opts.logFormat, "log-format", "text", "log format: text or json")
	flag.Parse()

	return opts
}

// validate checks the flags against the ranges they will be narrowed into.
//
// Checking the raw values is the whole point, and the listen port is the case that
// shows it: a port is a uint16 by the time anything binds it, so `-listen :99999` must
// fail here — quoting the number the operator actually typed — rather than being
// narrowed to 33465 and bound to a service nobody asked for. Clamp-then-validate reads
// as safe and is not. cmd/voxelheimd's -tick-rate is the same rule on a different flag.
func (o options) validate() error {
	if o.authDir == "" {
		return errors.New("the accounts directory must be named; this service has no ephemeral mode, " +
			"because an account nobody kept is a person who cannot get back in")
	}
	if o.listen == "" {
		return errors.New("the listen address must be named, as host:port")
	}

	_, port, err := net.SplitHostPort(o.listen)
	if err != nil {
		return fmt.Errorf("listen address %q is not host:port: %w", o.listen, err)
	}
	// A numeric port, rather than whatever /etc/services happens to resolve. The
	// refusal is deliberate: `-listen 127.0.0.1:htpt` is a typo far more often than it
	// is a service name, and a machine whose /etc/services differs is a machine where
	// the same flag binds a different port.
	number, err := strconv.Atoi(port)
	if err != nil {
		return fmt.Errorf("listen address %q needs a numeric port, got %q", o.listen, port)
	}
	if number < 0 || number > math.MaxUint16 {
		return fmt.Errorf("listen port must be in 0..%d, got %d", math.MaxUint16, number)
	}
	return nil
}

func newLogger(level, format string) (*slog.Logger, error) {
	var lvl slog.Level
	if err := lvl.UnmarshalText([]byte(level)); err != nil {
		return nil, fmt.Errorf("unknown log level %q", level)
	}

	handlerOpts := &slog.HandlerOptions{Level: lvl}
	switch strings.ToLower(format) {
	case "text":
		return slog.New(slog.NewTextHandler(os.Stderr, handlerOpts)), nil
	case "json":
		return slog.New(slog.NewJSONHandler(os.Stderr, handlerOpts)), nil
	default:
		return nil, fmt.Errorf("unknown log format %q", format)
	}
}

func run(ctx context.Context, opts options, log *slog.Logger) error {
	if err := opts.validate(); err != nil {
		return fmt.Errorf("invalid flags: %w", err)
	}

	// Before the listener, deliberately, in the order cmd/voxelheimd opens its world
	// in: the storage is the last thing that can refuse this configuration, and a
	// service that has already bound a port and answered a health probe is a worse
	// place to discover that its accounts directory cannot be created.
	//
	// The store is opened rather than held because nothing routed below reads an
	// account yet — there is no provider flow, so there is no request that arrives
	// holding a provider identity. What opening it does is the part that matters now:
	// it creates the directory, sweeps whatever a crash left mid-rename, refuses a
	// path it cannot use, and puts the format version in the log where an operator
	// looking at an old deployment can find it.
	accounts, err := auth.OpenStore(opts.authDir)
	if err != nil {
		return fmt.Errorf("opening the account store: %w", err)
	}
	log.Info("account store opened", "accounts_dir", accounts.Dir(), "format_version", auth.StoreVersion)

	// Beside the accounts, and before the listener for the same reason they are: a key
	// pair that cannot be read is a configuration this service cannot act on, and the
	// refusal is the point — regenerating over an unreadable pair would invalidate every
	// ticket in flight and every copy a game server has stored.
	keys, err := ticket.LoadOrCreate(opts.authDir)
	if err != nil {
		return fmt.Errorf("opening the ticket signing key: %w", err)
	}
	// **The public key is logged deliberately, and it is the only half that is.** An
	// operator has to be able to read it in order to give it to a game server, and a
	// value nobody can find is a value somebody copies out of the wrong place. The
	// private half cannot reach this line: ticket.Pair renders as its public key and
	// ticket.SigningKey redacts itself through every formatter there is.
	log.Info("ticket signing key ready",
		"algorithm", ticket.Algorithm,
		"public_key", keys.PublicHex(),
		"ticket_lifetime", ticket.Lifetime,
		"format_version", ticket.KeyStoreVersion)

	// Beside them both, and before the listener for the reason they are: a registry
	// directory this service cannot create is a configuration it cannot act on, and a
	// service that has already bound a port and answered a health probe is a worse place
	// to find that out.
	servers, err := registry.OpenStore(opts.authDir)
	if err != nil {
		return fmt.Errorf("opening the server registry: %w", err)
	}
	log.Info("server registry opened", "servers_dir", servers.Dir(), "format_version", registry.StoreVersion)

	// A key that is configured and unusable refuses here, before the port is bound, in the
	// order everything else in this function does. A key that is *absent* is not an error:
	// the registration route answers 503 and the list still works, because the list is read
	// with a ticket and not with this.
	registrationKey, err := loadRegistrationKey(opts.registrationKeyFile, log)
	if err != nil {
		return fmt.Errorf("configuring server registration: %w", err)
	}

	// Before the listener as well, and for the reason the store is: a redirect URI that
	// is not a URL is a configuration this service cannot act on, and discovering it
	// after the port is bound and the probes are answering is worse.
	signin, err := newSignIn(opts, accounts, log)
	if err != nil {
		return fmt.Errorf("configuring Discord sign-in: %w", err)
	}

	ln, err := net.Listen("tcp", opts.listen)
	if err != nil {
		return fmt.Errorf("listening on %s: %w", opts.listen, err)
	}

	svc := &service{log: log, keys: keys, signin: signin, servers: servers, registrationKey: registrationKey}

	// The address the listener actually bound rather than the one that was asked for,
	// which is the only way `-listen 127.0.0.1:0` tells anybody where it went.
	log.Info("voxelheim-auth listening", "addr", ln.Addr().String(), "accounts_dir", accounts.Dir())

	return svc.serve(ctx, ln)
}

// service is the HTTP surface. It is a type so that the route table below is a method
// and can reach whatever a handler needs, and so that serve can be driven in a test
// over a listener the test owns instead of only through a signal and a real port.
type service struct {
	log *slog.Logger

	// keys is the ticket signing key pair, and unlike signin below it is **never nil on
	// a service that is running**: run returns before building this value if the pair
	// cannot be read. There is no "tickets are not configured" mode to answer, because
	// there is nothing for an operator to configure — the pair is generated on first
	// start and kept.
	keys *ticket.Pair

	// signin is the Discord sign-in, and nil when this deployment has not been given a
	// Discord application. The routes exist either way — see newSignIn.
	signin *signIn

	// servers is the registry of game servers, and like keys it is **never nil on a service
	// that is running**: run returns before building this value if the directory cannot be
	// opened. There is no "the registry is not configured" mode, because there is nothing
	// for an operator to configure — the directory is created on first start and kept.
	servers *registry.Store

	// registrationKey is the credential a game server registers with, and nil when this
	// deployment has not been given one. Unlike servers above, that is a real state: the
	// key is a value an operator invents, so `POST /v1/servers` refuses with 503 until
	// there is one. Reading the list is unaffected — it is authenticated with a ticket.
	registrationKey *registry.Key
}

// route is one entry in this service's surface.
//
// The pattern carries its own method, which Go's own ServeMux has understood since
// 1.22: `GET /healthz` answers GET and HEAD and refuses everything else with a 405,
// so the method is part of the route rather than the first four lines of the handler.
type route struct {
	pattern string
	handler http.HandlerFunc
}

// routes is the whole surface of this service.
//
// **A table rather than a run of HandleFunc calls, and one place rather than several.**
// What can be reached over the network is the thing about a service that most deserves
// to be readable at a glance, and a registration that happens somewhere else is a route
// nobody reviewing this file knows about. TestTheRouteTableIsTheWholeSurface holds it
// to that: it drives the mux and asserts that what answers is exactly what is listed
// here.
func (s *service) routes() []route {
	return []route{
		{pattern: "GET /healthz", handler: s.health},
		{pattern: "GET /v1/ticket-key", handler: s.ticketKey},
		{pattern: "POST /v1/signin/discord/start", handler: s.signInStart},
		{pattern: "POST /v1/signin/discord/finish", handler: s.signInFinish},
		{pattern: "POST /v1/servers", handler: s.registerServer},
		{pattern: "GET /v1/servers", handler: s.listServers},
	}
}

func newMux(routes []route) *http.ServeMux {
	mux := http.NewServeMux()
	for _, r := range routes {
		mux.HandleFunc(r.pattern, r.handler)
	}
	return mux
}

// health reports that this process is up, and claims nothing else.
//
// **Liveness, not readiness, and the difference is deliberate.** It touches no disk: a
// health endpoint that stats the accounts directory on every probe is one that turns a
// monitoring interval into disk load, and one that a slow filesystem can make report a
// healthy service as dead. What could go wrong with the storage has already been asked
// once, at startup, where a failure refuses to bind rather than answering probes — so
// "the process is up" really is the whole of what this can honestly say.
func (s *service) health(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)

	// Fixed bytes rather than a marshal: the answer has no inputs, so encoding one
	// would be a way for it to acquire some.
	if _, err := io.WriteString(w, `{"status":"ok"}`+"\n"); err != nil {
		// Debug, and never an error: a probe that hangs up before reading the response
		// is a client-side event, and a monitoring system doing it every few seconds
		// would turn this into the loudest line in the log.
		s.log.Debug("the health response could not be written", "error", err)
	}
}

// serve answers on ln until ctx ends, then stops gracefully and returns.
//
// The listener is a parameter rather than something built in here, for the reason
// cmd/voxelheimd's server takes a transport: it is what lets a test bind
// 127.0.0.1:0, learn the port, and drive the real serving path.
func (s *service) serve(ctx context.Context, ln net.Listener) error {
	srv := &http.Server{
		Handler:           newMux(s.routes()),
		ReadHeaderTimeout: readHeaderTimeout,
		ReadTimeout:       readTimeout,
		WriteTimeout:      writeTimeout,
		IdleTimeout:       idleTimeout,
		// net/http insists on a *log.Logger for the errors it reports itself. This is
		// how it obeys "log/slog only" anyway: the adapter writes through the slog
		// handler, so a TLS handshake failure lands in the same stream, in the same
		// format, at a level chosen here rather than on stderr in a shape of its own.
		ErrorLog: slog.NewLogLogger(s.log.Handler(), slog.LevelWarn),
	}

	// Buffered, so the goroutine can always finish even if nothing reads this — an
	// unbuffered channel here leaks a goroutine on every early return below.
	served := make(chan error, 1)
	go func() {
		err := srv.Serve(ln)
		// The ordinary end of a graceful stop, not a fault.
		if errors.Is(err, http.ErrServerClosed) {
			err = nil
		}
		served <- err
	}()

	select {
	case err := <-served:
		// Serve gave up on its own — the listener died under it. Nothing to shut down.
		return err
	case <-ctx.Done():
	}

	// **WithoutCancel, and this is the trap it exists for**: ctx is already cancelled
	// by the time this line runs, so a timeout derived from it would expire the
	// instant it was created and turn every graceful shutdown into an immediate one.
	shutdownCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), shutdownTimeout)
	defer cancel()

	if err := srv.Shutdown(shutdownCtx); err != nil {
		return fmt.Errorf("shutting down: %w", err)
	}
	// Shutdown closes the listener, so Serve has returned or is about to. Waiting for
	// it is what makes this function's return mean "nothing of mine is still running".
	return <-served
}
