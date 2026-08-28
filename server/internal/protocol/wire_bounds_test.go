package protocol

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"testing"
)

// The bounds this package and the client both enforce, and the one place that checks
// they are the same number.
//
// Three constants in schemas/*.fbs are written as prose ("at most 32 bytes") rather than
// as anything flatc emits, so neither generated tree carries them and each side declares
// its own copy. That is the whole reason this file exists: a copied constant is only a
// contract while something compares the copies, and until now nothing did. Changing
// ResidentNameMaxBytes to 33 left every Go test green while the client went on refusing
// at 32 -- the encoder would have written a name the decoder closes the session over, and
// the first report would have been a player disconnecting next to a smith.
//
// **This is a text scan, not a build dependency.** It reads the client's source as data
// and never imports it; the Rust half stays the authority on its own value, and this only
// asks whether the two authorities agree. That is also its limit: it pins the declaration,
// not the enforcement, so a client that declared 32 and compared against something else
// would still pass here. What the client's own tests pin is that it refuses at
// RESIDENT_NAME_MAX_BYTES; what this pins is that RESIDENT_NAME_MAX_BYTES is this
// package's number.
func TestTheSharedBoundsAreOneNumberOnBothSidesOfTheWire(t *testing.T) {
	// An absent client workspace is nothing to verify, never an error -- the same rule
	// every script and CI job in this repository follows, because the workspaces are
	// scaffolded through the pipeline and a given ref may predate this one.
	codec := filepath.Join("..", "..", "..", "client", "src", "net", "codec.rs")
	source, err := os.ReadFile(codec)
	if os.IsNotExist(err) {
		t.Skip("the client workspace is not scaffolded at this ref; nothing to compare")
	}
	if err != nil {
		t.Fatalf("reading the client's codec: %v", err)
	}

	for _, bound := range []struct {
		rust string
		got  int
	}{
		{"MAX_MARKERS", MaxMarkers},
		{"MARKER_NOTE_MAX_BYTES", MarkerNoteMaxBytes},
		{"RESIDENT_NAME_MAX_BYTES", ResidentNameMaxBytes},
	} {
		// Anchored on the declaration rather than on any mention: the name appears in
		// doc comments and in error text throughout that file, and matching one of those
		// would pin a sentence instead of a number.
		pattern := regexp.MustCompile(
			fmt.Sprintf(`(?m)^pub const %s: usize = (\d+);`, regexp.QuoteMeta(bound.rust)),
		)
		found := pattern.FindAllSubmatch(source, -1)
		if len(found) != 1 {
			t.Errorf("%s is declared %d times in the client's codec, want exactly 1 -- "+
				"if it moved, this check has to move with it rather than be deleted",
				bound.rust, len(found))
			continue
		}
		want, err := strconv.Atoi(string(found[0][1]))
		if err != nil {
			t.Errorf("%s = %q, which is not a number", bound.rust, found[0][1])
			continue
		}
		if want != bound.got {
			t.Errorf("this package says %d and the client's %s says %d -- "+
				"one number written twice has to be the same number, and schemas/*.fbs "+
				"is what both of them copy", bound.got, bound.rust, want)
		}
	}
}
