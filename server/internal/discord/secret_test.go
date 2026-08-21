package discord

import (
	"bytes"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log/slog"
	"strings"
	"testing"
)

// The value a leak would have to carry, chosen so that finding it in a haystack is
// unambiguous: nothing else in a log line or a JSON document looks like this.
const fixtureSecret = "vxh-fixture-secret-6b1d9c4e"

// Every route a value can take out of this process, asserted against one secret.
//
// Four cases rather than one, because they are four mechanisms: fmt asks a Stringer,
// slog asks a LogValuer *before* its handler formats anything, and encoding/json asks
// a Marshaler and would otherwise write the string out verbatim. A type that covers
// only the first is the trap identity.Token documents, and this is the same trap in a
// string's clothing.
func TestASecretRedactsItselfOnEveryRouteOut(t *testing.T) {
	t.Parallel()

	secret := Secret(fixtureSecret)

	for name, got := range map[string]string{
		"String":      secret.String(),
		"%v":          fmt.Sprintf("%v", secret),
		"%s":          fmt.Sprintf("state=%s", secret),
		"%q":          fmt.Sprintf("%q", secret),
		"Sprint":      fmt.Sprint(secret),
		"in an error": fmt.Errorf("holding %s", secret).Error(),
		"LogValue":    secret.LogValue().String(),
	} {
		if strings.Contains(got, fixtureSecret) {
			t.Errorf("%s rendered the secret: %s", name, got)
		}
		if !strings.Contains(got, redacted) {
			t.Errorf("%s rendered %q, which is not the redaction", name, got)
		}
	}

	encoded, err := json.Marshal(struct {
		Held Secret `json:"held"`
	}{Held: secret})
	if err != nil {
		t.Fatalf("marshalling a struct holding a secret: %v", err)
	}
	if strings.Contains(string(encoded), fixtureSecret) {
		t.Errorf("encoding/json wrote the secret out: %s", encoded)
	}
}

// Both handlers, because the JSON one is the one a Stringer would not have saved.
func TestASecretDoesNotReachEitherLogHandler(t *testing.T) {
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
			log := slog.New(build(&out))
			secret := Secret(fixtureSecret)

			log.Info("a line that names a secret", "secret", secret)
			log.Info("a line that names a struct holding one", "held", struct{ S Secret }{S: secret})
			log.Error("a line that names an error built from one", "error", fmt.Errorf("holding %s", secret))

			logged := out.String()
			if logged == "" {
				t.Fatal("nothing was logged, so this test proves nothing")
			}
			for encoding, rendered := range map[string]string{
				"raw":       fixtureSecret,
				"hex":       hex.EncodeToString([]byte(fixtureSecret)),
				"base64":    base64.StdEncoding.EncodeToString([]byte(fixtureSecret)),
				"base64url": base64.RawURLEncoding.EncodeToString([]byte(fixtureSecret)),
			} {
				if strings.Contains(logged, rendered) {
					t.Errorf("the secret reached the %s log as %s", name, encoding)
				}
			}
		})
	}
}

// The two deliberate ways in and out. Unmarshalling is what puts an incoming state and
// code inside the type in the first place, and Reveal is the one named way back out.
func TestASecretIsReadableWhenItIsAskedForByName(t *testing.T) {
	t.Parallel()

	var req struct {
		Code Secret `json:"code"`
	}
	if err := json.Unmarshal([]byte(`{"code":"`+fixtureSecret+`"}`), &req); err != nil {
		t.Fatalf("unmarshalling into a secret: %v", err)
	}
	if req.Code.Reveal() != fixtureSecret {
		t.Errorf("the decoded secret is %q", req.Code.Reveal())
	}
	if req.Code.IsEmpty() {
		t.Error("a secret holding a value reports itself empty")
	}
	if !Secret("").IsEmpty() {
		t.Error("an empty secret does not report itself empty")
	}
}
