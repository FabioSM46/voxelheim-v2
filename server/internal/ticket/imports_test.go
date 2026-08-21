// The two boundaries this package's whole existence rests on, checked rather than
// asserted.
//
// internal/auth/imports_test.go carries the long form of the argument and the reason it
// is a test at all: an import is one line, it compiles, every gate stays green, and a
// sentence about what a package may reach quietly stops being true. The walks are
// deliberately not shared — hoisting one into a package both could import would mean
// creating a package that imports both sides of a boundary in order to check it.
//
// This one is narrower than internal/auth's and does not need that package's whole-module
// walk, because the claims here are stated the strong way round and are therefore about
// *this* package's own files:
//
//  1. **This package imports only internal/world from this module.** The game server is
//     going to import this package in order to verify a ticket, so anything reachable
//     from here is reachable from the simulation — an import of internal/auth would put
//     the accounts directory back inside the trust domain it was split out of, and
//     internal/auth's own test would not see it coming, because it would be this package
//     doing the importing rather than cmd/voxelheimd.
//  2. **Verification touches nothing.** Every file but the key store is held to an
//     allow-list of imports, none of which can open a file or a socket. That is the
//     property the design rests on: a game server holding the public key admits a player
//     without asking anybody, so the account service being down costs nobody a game.
//
// It reads the source rather than asking the toolchain, which keeps it hermetic: no
// subprocess, no module cache, no network, and it works in a checkout that has never
// been built.
package ticket

import (
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

const modulePath = "github.com/FabioSM46/voxelheim-v2/server"

// keyStoreFile is the one file in this package that is allowed to touch a disk. The
// split is load-bearing rather than cosmetic: it is what lets the assertion below be
// about a set of files instead of about a habit.
const keyStoreFile = "key.go"

// pureImports is everything a file that is not the key store may import.
//
// **An allow-list rather than a list of forbidden packages**, so it fails closed: a new
// import has to be added here deliberately, and nobody adds `net/http` to a list called
// pureImports without noticing what they are doing. Not one of these can open a file, a
// socket or a clock — time is the type of the parameter [Verify] takes, not a source of
// the current moment.
var pureImports = map[string]bool{
	"crypto/ed25519":  true,
	"crypto/sha256":   true,
	"encoding/base64": true,
	"encoding/binary": true,
	"encoding/hex":    true,
	"errors":          true,
	"fmt":             true,
	"log/slog":        true,
	"time":            true,
}

// sourceFile is one file's name and the imports it declares.
type sourceFile struct {
	name    string
	imports []string
}

// packageFiles parses this package's non-test files for their imports.
//
// The directory is ".", which is this package's own — a relative path, so nothing about
// this machine reaches the file.
func packageFiles(t *testing.T) []sourceFile {
	t.Helper()

	entries, err := os.ReadDir(".")
	if err != nil {
		t.Fatalf("reading this package's directory: %v", err)
	}

	fset := token.NewFileSet()
	var files []sourceFile
	for _, entry := range entries {
		name := entry.Name()
		if entry.IsDir() || !strings.HasSuffix(name, ".go") || strings.HasSuffix(name, "_test.go") {
			continue
		}
		parsed, err := parser.ParseFile(fset, name, nil, parser.ImportsOnly)
		if err != nil {
			t.Fatalf("parsing %s: %v", name, err)
		}
		file := sourceFile{name: name}
		for _, spec := range parsed.Imports {
			imported, err := strconv.Unquote(spec.Path.Value)
			if err != nil {
				t.Fatalf("reading an import path in %s: %v", name, err)
			}
			file.imports = append(file.imports, imported)
		}
		files = append(files, file)
	}

	// Fails closed. A walk that found nothing — a rename, a suffix rule that grew too
	// broad — would make every assertion below pass by describing an empty set, which is
	// the one way a boundary test can be worse than no test at all.
	seen := map[string]bool{}
	for _, file := range files {
		seen[file.name] = true
	}
	for _, want := range []string{"ticket.go", "verify.go", keyStoreFile} {
		if !seen[want] {
			t.Fatalf("the walk found no file named %s; this test is not looking at what it claims to", want)
		}
	}
	return files
}

// **The game server will import this package, so anything this package can reach, the
// simulation can reach.** internal/world is the single exception, and it is a narrow
// one: this package uses the five record helpers that package exports for the purpose —
// WriteAtomic, CheckHeader, CheckChecksum, PutChecksum, SweepTemporaries — rather than
// writing the same discipline down a fourth time. It never opens a world directory.
func TestThisPackageImportsOnlyTheRecordHelpers(t *testing.T) {
	t.Parallel()

	allowed := map[string]bool{modulePath + "/internal/world": true}

	for _, file := range packageFiles(t) {
		for _, imported := range file.imports {
			if !strings.HasPrefix(imported, modulePath) {
				continue
			}
			if !allowed[imported] {
				t.Errorf("%s imports %s; this package may import only internal/world from this module, "+
					"because the game server imports this package in order to verify", file.name, imported)
			}
		}
	}
}

// **Verification needs no I/O at all**, which is the property the whole design rests on:
// the game server verifies a signature instead of asking permission, so a small service
// being down does not stop a game running on a machine that is perfectly fine.
//
// Stated as a claim about imports because that is the form a test can hold. A behavioural
// test can show that one call did no I/O; this shows that none can.
func TestNothingButTheKeyStoreCanReachADiskOrASocket(t *testing.T) {
	t.Parallel()

	pure := 0
	for _, file := range packageFiles(t) {
		if file.name == keyStoreFile {
			// The key store is the one file that reads and writes, and it is not on the
			// verification path: a game server calls Verify and never LoadOrCreate.
			continue
		}
		pure++
		for _, imported := range file.imports {
			if !pureImports[imported] {
				t.Errorf("%s imports %s, which is not on the allow-list this package's verification path keeps; "+
					"if verification now needs it, the design has changed and this test is where to say so", file.name, imported)
			}
		}
	}
	if pure == 0 {
		t.Fatal("every file in this package was treated as the key store; this test asserted nothing")
	}
}

// **The number this package's layout is a consequence of.**
//
// schemas/handshake.fbs fixes `ClientHello.session_ticket` at 96 bytes and
// internal/protocol states the same number for the game server's decoder. This package
// cannot import that one — protocol is the simulation's, and the account service must
// not learn what a frame is — so the two constants are pinned to each other here
// instead, by reading the source rather than by linking against it.
//
// The pair matters because it is the acceptance criterion: a ticket's bytes are the ones
// that field carries. Two constants nobody compares are two constants that eventually
// differ, and the failure would be a handshake refusing every ticket this service mints.
func TestATicketIsTheLengthTheProtocolExpects(t *testing.T) {
	t.Parallel()

	const name = "SessionTicketLen"
	path := filepath.Join("..", "protocol", "envelope.go")

	parsed, err := parser.ParseFile(token.NewFileSet(), path, nil, 0)
	if err != nil {
		t.Fatalf("parsing %s: %v", path, err)
	}

	found := 0
	for _, decl := range parsed.Decls {
		general, ok := decl.(*ast.GenDecl)
		if !ok || general.Tok != token.CONST {
			continue
		}
		for _, spec := range general.Specs {
			value, ok := spec.(*ast.ValueSpec)
			if !ok {
				continue
			}
			for i, ident := range value.Names {
				if ident.Name != name || i >= len(value.Values) {
					continue
				}
				literal, ok := value.Values[i].(*ast.BasicLit)
				if !ok {
					t.Fatalf("%s.%s is not a literal this test can read", path, name)
				}
				declared, err := strconv.Atoi(literal.Value)
				if err != nil {
					t.Fatalf("%s.%s is %q, which is not a number: %v", path, name, literal.Value, err)
				}
				found++
				if declared != Size {
					t.Errorf("protocol.%s is %d and a ticket is %d bytes; the handshake would refuse every ticket this service mints",
						name, declared, Size)
				}
			}
		}
	}
	if found != 1 {
		t.Fatalf("found %d declarations of %s in %s, want exactly 1; this test is not reading what it claims to", found, name, path)
	}
}
