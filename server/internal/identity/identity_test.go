// Internal tests, deliberately: the domain separating a player id from every other
// digest in this repository is unexported — nothing outside needs to know what a player
// id is *made of* — and it is the one property here that a test cannot state from the
// outside without restating the implementation.
package identity

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log/slog"
	"strings"
	"testing"
)

// testAccount is a distinct account per seed, chosen rather than random so a failing
// test names the same player on every run.
func testAccount(seed byte) Account {
	var account Account
	for i := range account {
		account[i] = seed*31 + byte(i)
	}
	return account
}

func TestIDOfIsTheAccountUnderThisPackagesDomain(t *testing.T) {
	t.Parallel()

	account := testAccount(1)

	want := PlayerID(sha256.Sum256(append([]byte(playerIDDomain), account[:]...)))
	if got := IDOf(account); got != want {
		t.Errorf("IDOf = %s, want the domain-separated digest %s", got, want)
	}

	// **The digest is not the account's bare SHA-256**, which is the whole point of the
	// domain: nothing else in this repository that hashes sixteen bytes can produce a
	// player id, and a player id can never be mistaken for a value computed for another
	// purpose over the same account.
	if IDOf(account) == PlayerID(sha256.Sum256(account[:])) {
		t.Error("a player id is the account's bare SHA-256, so it shares a digest with every other use of one")
	}
}

func TestAPlayerIDIsStableAndDistinct(t *testing.T) {
	t.Parallel()

	account := testAccount(2)

	// Stable across calls and across restarts, which is what "recognised by a server
	// that has never seen you" needs from this side: the store keys on this, so an id
	// that varied would lose a player on every reconnection. Compared through a copy so
	// the linter reads two expressions rather than one repeated — which is the same
	// thing being asserted.
	again := account
	if IDOf(account) != IDOf(again) {
		t.Error("IDOf is not stable for one account")
	}

	// One bit is enough. The store is keyed by this, so two accounts sharing an id
	// would be two people sharing a life.
	other := account
	other[0] ^= 1
	if IDOf(account) == IDOf(other) {
		t.Error("two accounts one bit apart share a player id")
	}
}

// The id cannot be turned back into the account it names, which is what makes it safe
// in a log line and in a file name.
//
// Stated as the property a test can actually hold: a digest is one-way by construction
// and no test proves that, but the *shape* is checkable — the id is not the account, it
// does not contain the account, and it is not the account padded out to 32 bytes. Those
// are three ways of writing something that is called a hash and is not one.
func TestAPlayerIDDoesNotCarryItsAccount(t *testing.T) {
	t.Parallel()

	account := testAccount(3)
	id := IDOf(account)

	if strings.Contains(id.String(), hex.EncodeToString(account[:])) {
		t.Error("the player id contains its account in hex")
	}
	if strings.Contains(string(id[:]), string(account[:])) {
		t.Error("the player id contains its account's bytes")
	}
	padded := PlayerID(append(append([]byte{}, account[:]...), make([]byte, IDSize-AccountIDSize)...))
	if padded == id {
		t.Error("the player id is the account padded out rather than hashed")
	}
}

func TestPlayerIDFormatsForFileNamesAndForLogs(t *testing.T) {
	t.Parallel()

	id := IDOf(testAccount(4))

	full := id.String()
	if len(full) != 2*IDSize {
		t.Errorf("String is %d characters, want %d", len(full), 2*IDSize)
	}
	if decoded, err := hex.DecodeString(full); err != nil || PlayerID(decoded) != id {
		t.Errorf("String is not the id in hex: %v", err)
	}
	if strings.ToLower(full) != full {
		t.Error("String is not lowercase, so one id could name two files on a case-sensitive filesystem")
	}

	short := id.Short()
	if len(short) != 2*shortIDBytes {
		t.Errorf("Short is %d characters, want %d", len(short), 2*shortIDBytes)
	}
	if !strings.HasPrefix(full, short) {
		t.Errorf("Short %q is not a prefix of %q", short, full)
	}
}

// The rule the acceptance criterion states as "no account id reaches a log line", held
// as a property of the type rather than as a habit at every call site.
//
// Four routes out, and the last two are each one the others do not cover. The JSON
// handler would hand a [16]byte to encoding/json and write the account out as an array
// of 16 numbers, which a Stringer never sees; `%#v` walks the array by reflection and
// prints `0x1f, 0x3e, …`, which neither a Stringer nor a LogValuer sees — the route
// `ticket.Pair` leaked a signing key through while its own guard was green.
func TestAnAccountNeverRendersItsBytes(t *testing.T) {
	t.Parallel()

	account := testAccount(5)

	var text, jsonOut strings.Builder
	slog.New(slog.NewTextHandler(&text, nil)).Info("handshake", "account", account)
	slog.New(slog.NewJSONHandler(&jsonOut, nil)).Info("handshake", "account", account)

	marshalled, err := json.Marshal(account)
	if err != nil {
		t.Fatalf("marshalling the account: %v", err)
	}

	renderings := map[string]string{
		// %v and %#v, and not %s beside them: %s reaches the same Stringer %v does, so
		// it is the same route twice, and staticcheck is right that writing it out is
		// a Sprintf where a String call would do. %#v is the route neither covers.
		"%v":           fmt.Sprintf("%v", account),
		"%#v":          fmt.Sprintf("%#v", account),
		"String":       account.String(),
		"Sprint":       fmt.Sprint(account),
		"error string": fmt.Errorf("refusing %v", account).Error(),
		"slog text":    text.String(),
		"slog json":    jsonOut.String(),
		"json":         string(marshalled),
	}

	// Every shape the bytes could take on the way out. The raw form is checked too: a
	// renderer that wrote the array through fmt would put them there verbatim.
	asNumbers, err := json.Marshal([AccountIDSize]byte(account))
	if err != nil {
		t.Fatalf("marshalling the comparison value: %v", err)
	}
	leaks := map[string]string{
		"hex":             hex.EncodeToString(account[:]),
		"raw bytes":       string(account[:]),
		"a JSON array":    string(asNumbers),
		"%#v's byte list": fmt.Sprintf("%#v", [AccountIDSize]byte(account)),
	}

	for name, rendered := range renderings {
		if !strings.Contains(rendered, redacted) {
			t.Errorf("%s rendered %q, which does not redact the account", name, rendered)
		}
		for shape, leaked := range leaks {
			if strings.Contains(rendered, leaked) {
				// The leaked value is deliberately not quoted back: a failure means the
				// rendering holds an account, and this repository's CI log is public.
				t.Errorf("%s leaked the account as %s", name, shape)
			}
		}
	}
}

func TestIsZeroNamesTheAccountNoTicketMayCarry(t *testing.T) {
	t.Parallel()

	var none Account
	if !none.IsZero() {
		t.Error("the zero account does not report itself as zero")
	}
	if testAccount(6).IsZero() {
		t.Error("an account with bytes in it reports itself as zero")
	}
}
