// The boundary this package's contents make necessary, checked rather than asserted.
//
// internal/auth/imports_test.go carries the long form of the argument and the reason it is a
// test at all: an import is one line, it compiles, every gate stays green, and a sentence
// about what a package may reach quietly stops being true. The walks are deliberately not
// shared — hoisting one into a package both could import would mean creating a package that
// imports both sides of a boundary in order to check it.
//
// Two claims, in the shape internal/auth states its two:
//
//  1. **Only the account service imports this package.** A registry record holds the address
//     of somebody's house and the certificate a client will be told to trust; the game
//     server has no business opening that directory, and the announcing side talks to this
//     over HTTP and imports none of it. Stated the strong way round — nobody but
//     cmd/voxelheim-auth imports it at all — which makes the transitive question moot rather
//     than answered.
//  2. **This package imports internal/world and internal/ticket, and nothing else of ours.**
//     internal/world for the five record helpers it exports for the purpose, the way every
//     store here takes them. internal/ticket for one thing: `WorldIDFor`, so that a name this
//     store accepts is always a name a ticket can be minted for. That second import is the
//     alternative to a copy of the world-name rule, and a copy is what the pinned-constant
//     dance in internal/ticket's own imports_test exists for — needed there because those two
//     packages must not import each other, and not needed here because these may.
//
// It reads the source rather than asking the toolchain, which keeps it hermetic: no
// subprocess, no module cache, no network, and it works in a checkout that has never been
// built.
package registry

import (
	"go/parser"
	"go/token"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

const (
	modulePath      = "github.com/FabioSM46/voxelheim-v2/server"
	registryPackage = modulePath + "/internal/registry"

	// The one command that may open the registry directory, and the one that must not.
	authCommand = modulePath + "/cmd/voxelheim-auth"
	gameCommand = modulePath + "/cmd/voxelheimd"
)

// goFile is one source file's module-internal imports.
type goFile struct {
	pkg     string // the import path of the package it belongs to
	path    string // its path relative to server/, for a message a reader can act on
	isTest  bool
	imports []string
}

// packagePath is the import path of the package in dir, which is relative to server/.
func packagePath(dir string) string {
	if dir == "." {
		return modulePath
	}
	return modulePath + "/" + filepath.ToSlash(dir)
}

// moduleFiles parses every Go file under server/ for its imports of this module.
//
// The walk starts two directories up, which is server/ from server/internal/registry — a
// relative path, so nothing about this machine reaches the file.
func moduleFiles(t *testing.T) []goFile {
	t.Helper()

	root := filepath.Join("..", "..")
	if _, err := os.Stat(filepath.Join(root, "go.mod")); err != nil {
		t.Fatalf("the server workspace is not two directories up from this test: %v", err)
	}

	fset := token.NewFileSet()
	var files []goFile

	err := filepath.WalkDir(root, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() {
			// flatc output is regenerated rather than reasoned about, and testdata is
			// fixtures rather than code.
			if name := entry.Name(); name == "gen" || name == "testdata" {
				return fs.SkipDir
			}
			return nil
		}
		if !strings.HasSuffix(entry.Name(), ".go") {
			return nil
		}

		parsed, err := parser.ParseFile(fset, path, nil, parser.ImportsOnly)
		if err != nil {
			return err
		}

		rel, err := filepath.Rel(root, path)
		if err != nil {
			return err
		}
		file := goFile{
			pkg:    packagePath(filepath.Dir(rel)),
			path:   filepath.ToSlash(rel),
			isTest: strings.HasSuffix(entry.Name(), "_test.go"),
		}
		for _, spec := range parsed.Imports {
			imported, err := strconv.Unquote(spec.Path.Value)
			if err != nil {
				return err
			}
			if strings.HasPrefix(imported, modulePath) {
				file.imports = append(file.imports, imported)
			}
		}
		files = append(files, file)
		return nil
	})
	if err != nil {
		t.Fatalf("reading the server workspace: %v", err)
	}

	// Fails closed. A walk that found nothing — a rename, a skip rule that grew too broad —
	// would make every assertion below pass by describing an empty set, which is the one way
	// a boundary test can be worse than no test at all.
	seen := map[string]bool{}
	for _, file := range files {
		seen[file.pkg] = true
	}
	for _, want := range []string{registryPackage, authCommand, gameCommand} {
		if !seen[want] {
			t.Fatalf("the walk found no files for %s; this test is not looking at what it claims to", want)
		}
	}
	return files
}

// **cmd/voxelheimd must not import this package, and neither must anything else it can
// reach.** The announcing side of the registry — #105's half — is an outbound HTTP call and
// nothing more; the moment the simulation can open this directory, "the account service holds
// the registry" stops being true and a compromised game server is one that can read every
// other operator's address.
func TestOnlyTheAccountServiceImportsThisPackage(t *testing.T) {
	t.Parallel()

	for _, file := range moduleFiles(t) {
		for _, imported := range file.imports {
			if imported != registryPackage {
				continue
			}
			if file.pkg != authCommand {
				t.Errorf("%s imports %s; only %s may", file.path, registryPackage, authCommand)
			}
		}
	}
}

// The other half of the boundary, and the half that would be forgotten. Keeping the game
// server out of the registry is worth little if the registry reaches into the simulation:
// internal/game, internal/session and internal/persist are the game server's, and an import
// of any of them from here would put the account service inside the trust domain it was split
// out of.
//
// **internal/auth is on neither list and must stay off both.** This package and that one are
// two halves of one service and it would compile — what it would cost is internal/auth's own
// boundary test, which asserts that nobody but cmd/voxelheim-auth imports it. Passing the
// accounts through here would not trip that test, because the importer would be this package.
//
// Test files are exempt, which is the exemption internal/auth's version makes too: a test may
// reach for the real producer of a value in order to pin a format against it, and
// registry_test.go does exactly that with internal/certs.
func TestThisPackageImportsOnlyTheRecordHelpersAndTheWorldNameRule(t *testing.T) {
	t.Parallel()

	allowed := map[string]bool{
		modulePath + "/internal/world":  true,
		modulePath + "/internal/ticket": true,
	}

	for _, file := range moduleFiles(t) {
		if file.pkg != registryPackage || file.isTest {
			continue
		}
		for _, imported := range file.imports {
			if !allowed[imported] {
				t.Errorf("%s imports %s; this package may import only internal/world and internal/ticket from this module",
					file.path, imported)
			}
		}
	}
}
