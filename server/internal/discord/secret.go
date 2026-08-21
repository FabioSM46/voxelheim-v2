package discord

import (
	"encoding/json"
	"log/slog"
)

// redacted is what a [Secret] renders as, whichever formatter reaches it.
const redacted = "discord.Secret(redacted)"

// Secret is a value this service holds for the length of one sign-in and must never
// print: an authorization code, an access token, the PKCE verifier, or the state that
// names a sign-in in flight.
//
// A named string type rather than a string, for the reason identity.Account is a named
// array rather than a slice: the type is what stops the value from reaching a log by
// accident, and a plain string carries no such protection. The three defences are
// deliberately three, because each covers a route the others do not.
//
//   - [Secret.String] covers fmt, and therefore %v, %s, %q and every error message
//     built with fmt.Errorf.
//   - [Secret.LogValue] covers log/slog, which resolves a LogValuer before either
//     handler formats anything. Without it, -log-format json would hand the value to
//     encoding/json and write it out verbatim, which a Stringer never sees. This is
//     exactly the trap identity.Account documents, arriving here as a string rather
//     than as a byte array.
//   - [Secret.MarshalJSON] covers a struct that happens to hold one being marshalled
//     into a response or a diagnostic. The one place a secret is deliberately
//     serialised — the state handed back to the client, which has to cross the wire in
//     the clear or the flow cannot work — converts it explicitly through
//     [Secret.Reveal] into a plain string field, so redacting the default costs that
//     path nothing.
//
// Unmarshalling is deliberately left alone: a request body decodes straight into a
// Secret field, which is what puts the incoming state and code inside the type from
// the first moment this service holds them.
type Secret string

// Reveal is the value itself, and it is a named method so that every place a secret
// escapes the type is one grep away.
//
// A conversion — string(s) — does the same thing and cannot be prevented; what this
// buys is that the deliberate uses are findable and the accidental ones are the only
// ones that look like a conversion.
func (s Secret) Reveal() string { return string(s) }

// IsEmpty reports whether this secret holds nothing, without revealing what it holds.
func (s Secret) IsEmpty() bool { return s == "" }

// String redacts the secret, for fmt and for every error message built through it.
func (s Secret) String() string { return redacted }

// LogValue redacts a secret that reaches a log line. See the type comment: this is not
// the same defence as String, and the JSON handler is the reason.
func (s Secret) LogValue() slog.Value { return slog.StringValue(redacted) }

// MarshalJSON redacts a secret that reaches encoding/json.
func (s Secret) MarshalJSON() ([]byte, error) { return json.Marshal(redacted) }
