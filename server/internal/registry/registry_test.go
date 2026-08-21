package registry

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/certs"
)

// A fingerprint that is clearly synthetic and clearly well-formed: 64 lowercase hex
// characters that no certificate ever produced.
//
// **Every address and every digest in this package's tests is invented.** This repository
// is public and this package is about the addresses of people's houses, so the fixtures use
// TEST-NET-1 (192.0.2.0/24, reserved for documentation by RFC 5737) and the reserved
// `example.invalid` domain, and never a real host.
const (
	aFingerprint       = "abababababababababababababababababababababababababababababababab"
	anotherFingerprint = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"

	anAddress      = "192.0.2.10:7777"
	anotherAddress = "192.0.2.200:7777"
)

// aServer is a registration every field of which validates. The cases below mutate the one
// field under test rather than building a literal each time: a literal that omits a field is
// a case that passes for a reason it did not mean.
func aServer(now time.Time) Server {
	return Server{
		Name:        "midgard",
		DisplayName: "Midgard",
		Address:     anAddress,
		Fingerprint: aFingerprint,
		LastSeen:    now,
	}
}

func TestAValidRegistrationIsAccepted(t *testing.T) {
	t.Parallel()

	if err := aServer(time.Now()).Validate(); err != nil {
		t.Fatalf("a valid registration was refused: %v", err)
	}
}

// Every field, and the sentinel each refusal carries — because the endpoint in front of this
// store branches on those sentinels to tell an operator which line of their configuration is
// wrong. A refusal that carried the wrong sentinel would send them to the wrong line.
func TestValidateRefusesEachFieldWithItsOwnSentinel(t *testing.T) {
	t.Parallel()

	cases := map[string]struct {
		break_ func(*Server)
		want   error
	}{
		// The name is the world name, so this is ticket.WorldIDFor's vocabulary and not a
		// second copy of it.
		"no name":          {func(s *Server) { s.Name = "" }, ErrServerName},
		"a capital letter": {func(s *Server) { s.Name = "Midgard" }, ErrServerName},
		"a path traversal": {func(s *Server) { s.Name = "../../etc" }, ErrServerName},
		"an underscore":    {func(s *Server) { s.Name = "mid_gard" }, ErrServerName},
		"a name too long":  {func(s *Server) { s.Name = strings.Repeat("a", MaxNameBytes+1) }, ErrServerName},

		"no display name":         {func(s *Server) { s.DisplayName = "" }, ErrDisplayName},
		"a display name too long": {func(s *Server) { s.DisplayName = strings.Repeat("a", MaxDisplayNameBytes+1) }, ErrDisplayName},
		"a control character":     {func(s *Server) { s.DisplayName = "Mid\ngard" }, ErrDisplayName},
		"invalid UTF-8":           {func(s *Server) { s.DisplayName = "Mid\xffgard" }, ErrDisplayName},

		"no address":              {func(s *Server) { s.Address = "" }, ErrAddress},
		"an address with no port": {func(s *Server) { s.Address = "192.0.2.10" }, ErrAddress},
		"an address with no host": {func(s *Server) { s.Address = ":7777" }, ErrAddress},
		"a named port":            {func(s *Server) { s.Address = "192.0.2.10:voxelheim" }, ErrAddress},
		"a port past a uint16":    {func(s *Server) { s.Address = "192.0.2.10:99999" }, ErrAddress},
		"a zero port":             {func(s *Server) { s.Address = "192.0.2.10:0" }, ErrAddress},
		"an address with a space": {func(s *Server) { s.Address = "192.0.2.10 :7777" }, ErrAddress},
		"an address too long":     {func(s *Server) { s.Address = strings.Repeat("a", MaxAddressBytes) + ".invalid:7777" }, ErrAddress},

		// **The acceptance criterion, spelled out.** A registration is refused if the
		// fingerprint is not a well-formed digest.
		"no fingerprint":                {func(s *Server) { s.Fingerprint = "" }, ErrFingerprint},
		"a short fingerprint":           {func(s *Server) { s.Fingerprint = aFingerprint[:FingerprintHexLen-1] }, ErrFingerprint},
		"a long fingerprint":            {func(s *Server) { s.Fingerprint = aFingerprint + "a" }, ErrFingerprint},
		"a fingerprint in capitals":     {func(s *Server) { s.Fingerprint = strings.ToUpper(aFingerprint) }, ErrFingerprint},
		"a fingerprint that is not hex": {func(s *Server) { s.Fingerprint = strings.Repeat("z", FingerprintHexLen) }, ErrFingerprint},
		"a fingerprint with a colon":    {func(s *Server) { s.Fingerprint = "ab:" + aFingerprint[3:] }, ErrFingerprint},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			srv := aServer(time.Now())
			tc.break_(&srv)

			err := srv.Validate()
			if err == nil {
				t.Fatal("the registration was accepted")
			}
			// Both questions answer: "would this store refuse it" and "which field".
			if !errors.Is(err, ErrInvalidServer) {
				t.Errorf("the refusal %v does not wrap ErrInvalidServer", err)
			}
			if !errors.Is(err, tc.want) {
				t.Errorf("the refusal %v does not wrap %v", err, tc.want)
			}
		})
	}
}

// The one refusal no request can cause: [Store.Register] fills LastSeen from what its caller
// was given, and a zero would make the server permanently offline in every list that read it.
func TestARegistrationWithNoLastSeenIsRefused(t *testing.T) {
	t.Parallel()

	srv := aServer(time.Now())
	srv.LastSeen = time.Time{}
	if err := srv.Validate(); !errors.Is(err, ErrInvalidServer) {
		t.Errorf("a registration with no last-seen time answered %v, want ErrInvalidServer", err)
	}
}

// **The address is the one value this package will not put in a message.** It locates
// somebody's house, and an error string reaches a log. Every other field is quoted back on
// purpose: registration is authenticated, so the text is the operator's own and naming it is
// the difference between a mistake they can fix and one they have to guess at.
func TestARefusalNeverQuotesTheAddress(t *testing.T) {
	t.Parallel()

	for name, addr := range map[string]string{
		"an address with no port": anAddress[:len(anAddress)-5],
		"a named port":            "192.0.2.10:voxelheim",
		"a port past a uint16":    "192.0.2.10:99999",
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			srv := aServer(time.Now())
			srv.Address = addr
			err := srv.Validate()
			if err == nil {
				t.Fatal("the address was accepted")
			}
			// The host half is what locates a machine, so that is what must not appear.
			// The port number in the range message is deliberately allowed: it is a number
			// the operator typed and it points at nobody.
			if strings.Contains(err.Error(), "192.0.2.10") {
				t.Errorf("the refusal %q quotes the address", err)
			}
		})
	}
}

// **The number this list carries is the number `certs.Fingerprint` produces**, and this is
// the only place that is asserted rather than assumed.
//
// The acceptance criterion says the fingerprint in the list is the SHA-256 of the
// certificate the game server presents. This package deliberately computes no digest — a
// second way of arriving at the number is a second number — so what it can be held to is
// that the format it accepts is the format the real producer emits. A test-only import of
// the game server's certificate package is how that is checked against the producer instead
// of against this test's idea of one.
func TestTheFingerprintFormatIsTheOneCertsProduces(t *testing.T) {
	t.Parallel()

	// Ephemeral rather than LoadOrCreate: this needs a certificate, not a directory, and
	// nothing about the file layout is under test here.
	cert, err := certs.Ephemeral()
	if err != nil {
		t.Fatalf("certs.Ephemeral: %v", err)
	}
	fingerprint, err := certs.Fingerprint(cert)
	if err != nil {
		t.Fatalf("certs.Fingerprint: %v", err)
	}

	srv := aServer(time.Now())
	srv.Fingerprint = fingerprint
	if err := srv.Validate(); err != nil {
		t.Fatalf("the registry refused a fingerprint certs.Fingerprint produced: %v", err)
	}
	if len(fingerprint) != FingerprintHexLen {
		t.Errorf("certs.Fingerprint produced %d characters and this package expects %d",
			len(fingerprint), FingerprintHexLen)
	}
}

// Offline is a question about when the server last spoke, and `now` is a parameter so that a
// test can ask it about an hour ago without waiting one.
func TestOnlineIsAboutHowLongAgoTheServerSpoke(t *testing.T) {
	t.Parallel()

	now := time.Now()

	for name, tc := range map[string]struct {
		lastSeen time.Time
		want     bool
	}{
		"just now":                   {now, true},
		"a moment inside the window": {now.Add(-OfflineAfter + time.Second), true},
		"exactly at the window":      {now.Add(-OfflineAfter), false},
		"well past the window":       {now.Add(-time.Hour), false},
		// A clock that ran backwards, or an announce that arrived from a machine a little
		// ahead of this one. Online, which is the harmless direction.
		"a moment in the future": {now.Add(time.Minute), true},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			// Built inside the subtest rather than shared with the others. A value
			// declared outside a t.Parallel loop and written by every case is a data
			// race, and -race is what says so — which is the whole reason this package's
			// tests are worth running under it.
			srv := aServer(tc.lastSeen)
			if got := srv.Online(now); got != tc.want {
				t.Errorf("Online is %v, want %v", got, tc.want)
			}
		})
	}
}

// **The key is never held, only its digest**, which is what makes "the registration key is
// never logged" a property of the type rather than a rule every call site remembers.
func TestAKeyMatchesItselfAndNothingElse(t *testing.T) {
	t.Parallel()

	const raw = "a-registration-key-long-enough-to-be-accepted"
	key, err := ParseKey(raw)
	if err != nil {
		t.Fatalf("ParseKey: %v", err)
	}

	if !key.Matches(raw) {
		t.Error("the key does not match itself")
	}
	for name, wrong := range map[string]string{
		"a different key":      "another-registration-key-long-enough-to-pass",
		"a prefix":             raw[:len(raw)-1],
		"a suffix":             raw + "x",
		"empty":                "",
		"whitespace around it": " " + raw + " ",
	} {
		if key.Matches(wrong) {
			t.Errorf("the key matched %s", name)
		}
	}
}

// Surrounding whitespace is removed, because `echo key > key-file` leaves a newline and an
// operator who had to notice that would notice it as an authentication failure with nothing
// in any log to explain it. **Whitespace inside is refused rather than removed**: a key that
// cannot be put in an `Authorization` header is one no announcement could ever present.
func TestAKeyIsTrimmedAtTheEndsAndRefusedInTheMiddle(t *testing.T) {
	t.Parallel()

	const raw = "a-registration-key-long-enough-to-be-accepted"

	trimmed, err := ParseKey("\n\t " + raw + " \n")
	if err != nil {
		t.Fatalf("a key with surrounding whitespace was refused: %v", err)
	}
	if !trimmed.Matches(raw) {
		t.Error("trimming produced a key that does not match the untrimmed value")
	}

	for name, bad := range map[string]string{
		"a newline in the middle": raw[:10] + "\n" + raw[10:],
		"a space in the middle":   raw[:10] + " " + raw[10:],
		"a tab in the middle":     raw[:10] + "\t" + raw[10:],
		"a non-ASCII character":   raw[:10] + "é" + raw[10:],
	} {
		if _, err := ParseKey(bad); !errors.Is(err, ErrInvalidKey) {
			t.Errorf("%s answered %v, want ErrInvalidKey", name, err)
		}
	}
}

// A short key is the one thing this package can do about an operator who would otherwise use
// a word: there is no rate limit here and none is coming, so the bound on guessing is the
// key's length.
func TestAShortOrEmptyKeyIsRefused(t *testing.T) {
	t.Parallel()

	for name, bad := range map[string]string{
		"empty":                  "",
		"only whitespace":        "   \n",
		"a word":                 "hunter2",
		"one short of the bound": strings.Repeat("a", MinKeyBytes-1),
	} {
		if _, err := ParseKey(bad); !errors.Is(err, ErrInvalidKey) {
			t.Errorf("%s answered %v, want ErrInvalidKey", name, err)
		}
	}
	if _, err := ParseKey(strings.Repeat("a", MinKeyBytes)); err != nil {
		t.Errorf("a key exactly at the bound was refused: %v", err)
	}
}

// **A credential longer than a key can be is refused before it is hashed, and no key can
// be that long**, which are the two halves of one bound and are tested together because
// either half alone is a bug.
//
// The presentation half is what the bound is for: this endpoint is reachable by anybody,
// because a credential has to be read to be refused, and an `Authorization` header is as
// long as whoever sent it chose — a megabyte, by net/http's default. Hashing it is work an
// unauthenticated request should not be able to buy.
//
// The measurement is allocation rather than a value, because both versions answer false: a
// wrong key is a wrong key whatever its length. What distinguishes them is that hashing a
// megabyte has to copy it first, and refusing on length copies nothing. So this fails
// against a Matches that hashes whatever it is given, which is the version it was written
// against.
//
// No t.Parallel here: testing.AllocsPerRun pins GOMAXPROCS to 1 for its measurement and
// says in as many words not to run it alongside parallel tests.
func TestAnOversizedCredentialIsRefusedWithoutBeingHashed(t *testing.T) {
	key, err := ParseKey(strings.Repeat("a", MinKeyBytes))
	if err != nil {
		t.Fatalf("ParseKey: %v", err)
	}

	oversized := strings.Repeat("a", 1<<20)
	if key.Matches(oversized) {
		t.Error("a megabyte of text matched the key")
	}
	if allocs := testing.AllocsPerRun(50, func() { key.Matches(oversized) }); allocs != 0 {
		t.Errorf("Matches allocated %v times for a credential it can refuse on length alone", allocs)
	}

	// The other half. Were a key allowed to be longer than the bound, the refusal above
	// would eventually be turning away a key an operator had configured — an authentication
	// failure with nothing in any log to explain it, which is the failure the whitespace
	// trimming exists to prevent, arriving by a different door.
	if _, err := ParseKey(strings.Repeat("a", MaxKeyBytes+1)); !errors.Is(err, ErrInvalidKey) {
		t.Errorf("a key one over the bound answered %v, want ErrInvalidKey", err)
	}
	if _, err := ParseKey(strings.Repeat("a", MaxKeyBytes)); err != nil {
		t.Errorf("a key exactly at the bound was refused: %v", err)
	}
	if MaxKeyBytes <= MinKeyBytes {
		t.Errorf("MaxKeyBytes (%d) leaves no room above MinKeyBytes (%d)", MaxKeyBytes, MinKeyBytes)
	}
}

// **The key never reaches a message**, which is asserted here rather than left to the four
// redaction methods: a refusal is the one place a value is most likely to be echoed.
func TestAKeyRefusalNeverQuotesTheKey(t *testing.T) {
	t.Parallel()

	const secret = "a-registration-key-with-a\nnewline-in-the-middle-of-it"
	_, err := ParseKey(secret)
	if err == nil {
		t.Fatal("the key was accepted")
	}
	if strings.Contains(err.Error(), "a-registration-key") {
		t.Errorf("the refusal %q quotes the key", err)
	}
}

// The four redaction routes, each covering one the others do not — `ticket.SigningKey`'s
// argument, and the reason it is repeated here is that a Key is a struct with an unexported
// field, so fmt would otherwise print the digest as a list of numbers.
func TestAKeyRedactsThroughEveryFormatter(t *testing.T) {
	t.Parallel()

	key, err := ParseKey("a-registration-key-long-enough-to-be-accepted")
	if err != nil {
		t.Fatalf("ParseKey: %v", err)
	}

	// The digest of the fixture, as the bytes any of these would print if redaction failed.
	// Looked for rather than assumed absent: a test searching for the wrong value passes
	// while proving nothing.
	digest := fmt.Sprintf("%v", key.digest)

	// Each verb built the way a message actually is, which is `ticket.SigningKey`'s own
	// test's shape: a bare Sprintf("%s", x) on a Stringer is the same call as x.String() and
	// staticcheck says so, while a key embedded in a sentence is the thing that really
	// happens and is the thing worth checking.
	for name, rendered := range map[string]string{
		"%v":                     fmt.Sprintf("%v", key),
		"%s":                     fmt.Sprintf("a key: %s", key),
		"%#v":                    fmt.Sprintf("%#v", key),
		"an error built with it": fmt.Errorf("a message: %v", key).Error(),
		// A struct that happens to hold one, which is how a value most often reaches a
		// formatter without anybody meaning it to.
		"a struct holder": fmt.Sprintf("%v", struct{ K Key }{key}),
	} {
		if !strings.Contains(rendered, redactedKey) {
			t.Errorf("%s rendered %q, which is not the redaction", name, rendered)
		}
		if strings.Contains(rendered, digest) {
			t.Errorf("%s rendered the key's digest", name)
		}
	}

	marshalled, err := json.Marshal(key)
	if err != nil {
		t.Fatalf("marshalling a key: %v", err)
	}
	if !strings.Contains(string(marshalled), redactedKey) {
		t.Errorf("encoding/json wrote %s, which is not the redaction", marshalled)
	}

	// slog resolves a LogValuer before either handler formats anything, which is the route
	// a Stringer never sees: the JSON handler would otherwise hand the struct to
	// encoding/json, and the text handler would hand it to fmt.
	for name, build := range map[string]func(*bytes.Buffer) slog.Handler{
		"text": func(w *bytes.Buffer) slog.Handler { return slog.NewTextHandler(w, nil) },
		"json": func(w *bytes.Buffer) slog.Handler { return slog.NewJSONHandler(w, nil) },
	} {
		var out bytes.Buffer
		slog.New(build(&out)).Info("a line", "key", key)
		if !strings.Contains(out.String(), redactedKey) {
			t.Errorf("the %s handler wrote %q, which is not the redaction", name, out.String())
		}
		if strings.Contains(out.String(), digest) {
			t.Errorf("the %s handler wrote the key's digest", name)
		}
	}
}
