// The boundary this package sits on, checked rather than asserted.
//
// internal/auth/imports_test.go carries the long form of this argument and the reason
// it is a test at all: an import is one line, it compiles, every gate stays green, and
// a sentence about what a package may reach quietly stops being true. The two walks are
// deliberately not shared. Hoisting one into a package both could import would mean
// creating a package that imports both sides of a boundary in order to check it, and
// the boundary is the thing being checked.
//
// It reads the source rather than asking the toolchain, which keeps it hermetic: no
// subprocess, no module cache, no network, and it works in a checkout that has never
// been built.
package discord

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
	modulePath     = "github.com/FabioSM46/voxelheim-v2/server"
	discordPackage = modulePath + "/internal/discord"

	// The one command that may run a sign-in, and the one that must not.
	authCommand = modulePath + "/cmd/voxelheim-auth"
	gameCommand = modulePath + "/cmd/voxelheimd"
)

// sourceFile is one file's package path, its path relative to server/, and the imports
// it takes from this module.
type sourceFile struct {
	pkg     string
	path    string
	imports []string
}

// moduleFiles parses every Go file under server/ for its imports of this module.
//
// The walk starts two directories up, which is server/ from server/internal/discord —
// a relative path, so nothing about this machine reaches the file.
func moduleFiles(t *testing.T) []sourceFile {
	t.Helper()

	root := filepath.Join("..", "..")
	if _, err := os.Stat(filepath.Join(root, "go.mod")); err != nil {
		t.Fatalf("the server workspace is not two directories up from this test: %v", err)
	}

	fset := token.NewFileSet()
	var files []sourceFile

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

		dir := filepath.ToSlash(filepath.Dir(rel))
		pkg := modulePath
		if dir != "." {
			pkg = modulePath + "/" + dir
		}
		file := sourceFile{pkg: pkg, path: filepath.ToSlash(rel)}
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

	// Fails closed. A walk that found nothing — a rename, a skip rule that grew too
	// broad — would make every assertion below pass by describing an empty set, which
	// is the one way a boundary test can be worse than no test at all.
	seen := map[string]bool{}
	for _, file := range files {
		seen[file.pkg] = true
	}
	for _, want := range []string{discordPackage, authCommand, gameCommand} {
		if !seen[want] {
			t.Fatalf("the walk found no files for %s; this test is not looking at what it claims to", want)
		}
	}
	return files
}

// **The game server must not talk to Discord, and neither must anything it can reach.**
// Stated the strong way round — nobody but the account service imports this package at
// all — which makes the transitive question moot rather than answered.
func TestOnlyTheAccountServiceImportsThisPackage(t *testing.T) {
	t.Parallel()

	for _, file := range moduleFiles(t) {
		for _, imported := range file.imports {
			if imported != discordPackage {
				continue
			}
			if file.pkg != authCommand {
				t.Errorf("%s imports %s; only %s may", file.path, discordPackage, authCommand)
			}
		}
	}
}

// The other half, and the half that would be forgotten. This package is a leaf in the
// shape internal/identity is one: it never opens the accounts directory, never learns
// what an account is, and never imports internal/auth — which is what keeps a provider
// flow testable against an httptest.Server and an account store testable against a
// directory, with cmd/voxelheim-auth the one place the two meet.
func TestThisPackageImportsNothingOfOurs(t *testing.T) {
	t.Parallel()

	for _, file := range moduleFiles(t) {
		if file.pkg != discordPackage {
			continue
		}
		for _, imported := range file.imports {
			t.Errorf("%s imports %s; this package imports nothing from this module", file.path, imported)
		}
	}
}
