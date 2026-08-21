package auth

import (
	"errors"
	"strings"
	"testing"
	"unicode/utf8"
)

// The identities this service will key on, and the ones it refuses. The refusals are
// the interesting half: every one of them is a way for two spellings of one person to
// become two accounts, or for one spelling to name two people.
func TestProviderIdentityValidate(t *testing.T) {
	t.Parallel()

	sound := map[string]ProviderIdentity{
		"a provider and a subject":       {Provider: "discord", Subject: "90000000000000001"},
		"a hyphenated provider":          {Provider: "some-provider", Subject: "9"},
		"digits in the provider name":    {Provider: "oidc2", Subject: "9"},
		"a subject at the cap":           {Provider: "discord", Subject: strings.Repeat("9", MaxSubjectBytes)},
		"a provider at the cap":          {Provider: strings.Repeat("d", MaxProviderBytes), Subject: "9"},
		"a subject that is not a number": {Provider: "discord", Subject: "a-uuid-shaped-subject"},
	}
	for name, id := range sound {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			if err := id.Validate(); err != nil {
				t.Errorf("a sound identity was refused: %v", err)
			}
		})
	}

	refused := map[string]ProviderIdentity{
		"nothing at all":                {},
		"no provider":                   {Subject: "9"},
		"no subject":                    {Provider: "discord"},
		"a capital in the provider":     {Provider: "Discord", Subject: "9"},
		"a space in the provider":       {Provider: "some provider", Subject: "9"},
		"an underscore in the provider": {Provider: "some_provider", Subject: "9"},
		"a provider past the cap":       {Provider: strings.Repeat("d", MaxProviderBytes+1), Subject: "9"},
		"a subject past the cap":        {Provider: "discord", Subject: strings.Repeat("9", MaxSubjectBytes+1)},
		"a subject that is not UTF-8":   {Provider: "discord", Subject: string([]byte{0xff, 0xfe})},
	}
	for name, id := range refused {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			err := id.Validate()
			if !errors.Is(err, ErrInvalidIdentity) {
				t.Fatalf("Validate = %v, want ErrInvalidIdentity", err)
			}
			// The rule is stated; the value never is. An error string reaches a log,
			// and a provider subject is somebody's identity arriving from a third
			// party — quoting it would paste remote text into the log besides.
			if id.Subject != "" && strings.Contains(err.Error(), id.Subject) {
				t.Error("the refusal quoted the subject back into its error message")
			}
		})
	}
}

// A capitalised provider name is refused rather than lowercased, and this is the
// reason: normalising would accept both spellings as one provider, which is right
// until something compares them before they reach here — and then two spellings are
// two accounts for one person, with nothing to notice it by.
func TestTwoSpellingsOfAProviderCannotBecomeTwoAccounts(t *testing.T) {
	t.Parallel()

	lower := ProviderIdentity{Provider: "discord", Subject: "90000000000000001"}
	upper := ProviderIdentity{Provider: "Discord", Subject: "90000000000000001"}

	if err := lower.Validate(); err != nil {
		t.Fatalf("the lowercase spelling was refused: %v", err)
	}
	if err := upper.Validate(); !errors.Is(err, ErrInvalidIdentity) {
		t.Fatalf("Validate on the capitalised spelling = %v, want ErrInvalidIdentity", err)
	}
}

// A mint that fails is returned, never swallowed. The alternative is the one value
// this package must never produce: a zero id, which every account that failed to mint
// would share, and which would make them all the same person.
func TestNewAccountIDRefusesAFailedMint(t *testing.T) {
	t.Parallel()

	id, err := newAccountID(strings.NewReader("too few bytes"))
	if err == nil {
		t.Fatal("a short random source produced an account id")
	}
	if !id.IsZero() {
		t.Errorf("a failed mint handed back %s rather than nothing", id)
	}
}

func TestAnAccountIDIsHexAndNotZero(t *testing.T) {
	t.Parallel()

	id, err := NewAccountID()
	if err != nil {
		t.Fatalf("NewAccountID: %v", err)
	}
	if id.IsZero() {
		t.Fatal("a minted account id is zero")
	}
	if got := len(id.String()); got != 2*AccountIDSize {
		t.Errorf("an account id renders as %d characters, want %d", got, 2*AccountIDSize)
	}

	// Two mints are two accounts. A generator that answered the same bytes twice would
	// hand one person's account to another.
	other, err := NewAccountID()
	if err != nil {
		t.Fatalf("NewAccountID: %v", err)
	}
	if other == id {
		t.Error("two mints produced the same account id")
	}
}

// The cut is at a rune boundary, so what is stored is still the text it was a prefix
// of — a cut through the middle of a multi-byte rune stores something that no longer
// decodes.
func TestTruncateNameCutsAtARuneBoundary(t *testing.T) {
	t.Parallel()

	for name, in := range map[string]string{
		"a name under the cap":       "Eivor",
		"a name exactly at the cap":  strings.Repeat("n", MaxDisplayNameBytes),
		"a long ASCII name":          strings.Repeat("n", MaxDisplayNameBytes*2),
		"a long two-byte-rune name":  strings.Repeat("á", MaxDisplayNameBytes),
		"a long four-byte-rune name": strings.Repeat("𐌰", MaxDisplayNameBytes),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			got := truncateName(in)
			if len(got) > MaxDisplayNameBytes {
				t.Errorf("truncateName left %d bytes, more than the %d the format keeps", len(got), MaxDisplayNameBytes)
			}
			if !strings.HasPrefix(in, got) {
				t.Error("the truncation is not a prefix of what was given")
			}
			if !utf8.ValidString(got) {
				t.Error("the truncation cut through a rune")
			}
		})
	}
}
