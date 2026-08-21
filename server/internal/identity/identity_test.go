// Internal tests, deliberately: newToken's failure branch is the one thing here
// that cannot be reached from outside — crypto/rand does not fail on any platform
// this server runs on — and it is exactly the branch that must never be allowed to
// produce a zero token.
package identity

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"testing"
)

func TestNewTokenMintsDistinctTokens(t *testing.T) {
	t.Parallel()

	seen := make(map[Token]struct{}, 64)
	for range 64 {
		token, err := NewToken()
		if err != nil {
			t.Fatalf("NewToken: %v", err)
		}
		if token == (Token{}) {
			t.Fatal("NewToken returned the zero token")
		}
		if _, repeat := seen[token]; repeat {
			t.Fatal("NewToken returned the same token twice")
		}
		seen[token] = struct{}{}
	}
}

// failingReader is a crypto/rand that has stopped working.
type failingReader struct{ err error }

func (f failingReader) Read([]byte) (int, error) { return 0, f.err }

// shortReader answers fewer bytes than asked for and then stops, which is the other
// way a source of randomness fails: not an error on the first call, but a token that
// is only partly random.
type shortReader struct{ n int }

func (s *shortReader) Read(p []byte) (int, error) {
	if s.n <= 0 {
		return 0, errors.New("out of entropy")
	}
	n := min(s.n, len(p))
	for i := range p[:n] {
		p[i] = 0xAB
	}
	s.n -= n
	return n, nil
}

func TestNewTokenRefusesAFailedRead(t *testing.T) {
	t.Parallel()

	// The rule the AC states: a failed read is a refusal, never a zero token. A zero
	// token would be shared by every session that failed to mint one, which makes them
	// all the same player — the one outcome worse than a refused handshake.
	sources := map[string]interface{ Read([]byte) (int, error) }{
		"a source that errors":          failingReader{err: errors.New("no entropy")},
		"a source that stops part way":  &shortReader{n: TokenSize - 1},
		"a source that answers nothing": &shortReader{n: 0},
		"a source that errors with EOF": failingReader{err: fmt.Errorf("EOF")},
	}

	for name, source := range sources {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			token, err := newToken(source)
			if err == nil {
				t.Fatal("newToken accepted a broken source of randomness")
			}
			if token != (Token{}) {
				t.Error("newToken returned a partly-filled token beside its error")
			}
		})
	}
}

func TestTokenFromRefusesEveryLengthButThirtyTwo(t *testing.T) {
	t.Parallel()

	for _, size := range []int{0, 1, 7, 31, 33, 64} {
		if _, err := TokenFrom(make([]byte, size)); !errors.Is(err, ErrTokenSize) {
			t.Errorf("TokenFrom(%d bytes) = %v, want ErrTokenSize", size, err)
		}
	}

	source := make([]byte, TokenSize)
	source[0] = 9
	token, err := TokenFrom(source)
	if err != nil {
		t.Fatalf("TokenFrom(32 bytes): %v", err)
	}

	// The copy is the point: source is usually a decoded frame, and a token that
	// aliased it would change underneath whoever held it.
	source[0] = 200
	if token[0] != 9 {
		t.Error("TokenFrom aliased the slice it was given instead of copying it")
	}
}

func TestIDOfIsTheTokensSHA256(t *testing.T) {
	t.Parallel()

	token, err := NewToken()
	if err != nil {
		t.Fatalf("NewToken: %v", err)
	}

	if got, want := IDOf(token), PlayerID(sha256.Sum256(token[:])); got != want {
		t.Errorf("IDOf = %s, want the token's SHA-256 %s", got, want)
	}
	// Stable across calls: the store keys on this, so an id that varied would lose a
	// player on every reconnection. Compared through a copy so the linter reads two
	// expressions rather than one repeated — which is the same thing being asserted.
	again := token
	if IDOf(token) != IDOf(again) {
		t.Error("IDOf is not stable for one token")
	}

	// One bit is enough. The store is keyed by this, so two tokens sharing an id would
	// be two players sharing a record.
	other := token
	other[0] ^= 1
	if IDOf(token) == IDOf(other) {
		t.Error("two tokens one bit apart share a player id")
	}
}

func TestPlayerIDFormatsForFileNamesAndForLogs(t *testing.T) {
	t.Parallel()

	id := IDOf(Token{1, 2, 3})

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

func TestATokenNeverRendersItsBytes(t *testing.T) {
	t.Parallel()

	token, err := NewToken()
	if err != nil {
		t.Fatalf("NewToken: %v", err)
	}

	// Every renderer that could reach a log line or an error string. The JSON handler
	// is the one that String alone does not cover: without LogValue it would hand the
	// [32]byte to encoding/json and write the token out as an array of 32 numbers.
	var text, jsonOut strings.Builder
	slog.New(slog.NewTextHandler(&text, nil)).Info("handshake", "token", token)
	slog.New(slog.NewJSONHandler(&jsonOut, nil)).Info("handshake", "token", token)

	renderings := map[string]string{
		"%v":           fmt.Sprintf("%v", token),
		"String":       token.String(),
		"Sprint":       fmt.Sprint(token),
		"error string": fmt.Errorf("refusing %v", token).Error(),
		"slog text":    text.String(),
		"slog json":    jsonOut.String(),
	}

	for name, rendered := range renderings {
		if !strings.Contains(rendered, redacted) {
			t.Errorf("%s rendered %q, which does not redact the token", name, rendered)
		}
		if strings.Contains(rendered, hex.EncodeToString(token[:])) {
			t.Errorf("%s leaked the token as hex", name)
		}
		// The JSON handler's shape, had LogValue not caught it: the bytes as numbers.
		asNumbers, err := json.Marshal([TokenSize]byte(token))
		if err != nil {
			t.Fatalf("marshalling the comparison value: %v", err)
		}
		if strings.Contains(rendered, string(asNumbers)) {
			t.Errorf("%s leaked the token as a JSON array of bytes", name)
		}
	}
}

func TestEqualComparesTokens(t *testing.T) {
	t.Parallel()

	token, err := NewToken()
	if err != nil {
		t.Fatalf("NewToken: %v", err)
	}
	if !token.Equal(token) {
		t.Error("a token does not equal itself")
	}

	other := token
	other[TokenSize-1] ^= 0x80
	if token.Equal(other) {
		t.Error("two different tokens compared equal")
	}
}
