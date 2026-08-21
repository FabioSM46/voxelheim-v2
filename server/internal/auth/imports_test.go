// The boundary this package's whole existence rests on, checked rather than asserted.
//
// The account service and the game server are separate trust domains that happen to
// ship from one Go module. Nothing in the module enforces that on its own: an import
// is one line, it compiles, every gate stays green, and the sentence "the account
// service holds the accounts" quietly stops being true. So the boundary is a test —
// the same move this repository makes wherever a claim would otherwise be prose
// nobody executes.
//
// It reads the source rather than asking the toolchain, which keeps it hermetic: no
// subprocess, no module cache, no network, and it works in a checkout that has never
// been built.
package auth

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
	modulePath  = "github.com/FabioSM46/voxelheim-v2/server"
	authPackage = modulePath + "/internal/auth"

	// The one command that may open the accounts directory, and the one that must not.
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
// The walk starts two directories up, which is server/ from server/internal/auth —
// a relative path, so nothing about this machine reaches the file.
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

	// Fails closed. A walk that found nothing — a rename, a skip rule that grew too
	// broad — would make every assertion below pass by describing an empty set, which
	// is the one way a boundary test can be worse than no test at all.
	seen := map[string]bool{}
	for _, file := range files {
		seen[file.pkg] = true
	}
	for _, want := range []string{authPackage, authCommand, gameCommand} {
		if !seen[want] {
			t.Fatalf("the walk found no files for %s; this test is not looking at what it claims to", want)
		}
	}
	return files
}

// **cmd/voxelheimd must not import this package, and neither must anything else it
// can reach.** The assertion is stated the strong way round — nobody but the account
// service imports internal/auth at all — which makes the transitive question moot
// rather than answered: a package the game server imports cannot pass this package on
// if it never held it.
func TestOnlyTheAccountServiceImportsThisPackage(t *testing.T) {
	t.Parallel()

	for _, file := range moduleFiles(t) {
		for _, imported := range file.imports {
			if imported != authPackage {
				continue
			}
			if file.pkg != authCommand {
				t.Errorf("%s imports %s; only %s may", file.path, authPackage, authCommand)
			}
		}
	}
}

// The other half of the boundary, and the half that would be forgotten. Keeping the
// game server out of the accounts directory is worth little if the account service
// reaches into the simulation: internal/game, internal/session and internal/persist
// are the game server's, and an import of any of them from here would put the
// account service inside the trust domain it was split out of.
//
// internal/world is the single exception, and it is a narrow one: this package uses
// the five record helpers that package exports for the purpose — WriteAtomic,
// CheckHeader, CheckChecksum, PutChecksum, SweepTemporaries — rather than writing the
// same discipline down a third time. It never opens a world directory.
func TestThisPackageImportsNothingOfTheGameServers(t *testing.T) {
	t.Parallel()

	allowed := map[string]bool{
		modulePath + "/internal/world": true,
	}

	for _, file := range moduleFiles(t) {
		if file.pkg != authPackage || file.isTest {
			continue
		}
		for _, imported := range file.imports {
			if !allowed[imported] {
				t.Errorf("%s imports %s; this package may import only internal/world from this module", file.path, imported)
			}
		}
	}
}
