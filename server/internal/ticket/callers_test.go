// The one claim [VerifyAnyWorld]'s doc makes that a comment cannot keep.
//
// That function drops the world comparison, and its doc says a game server must never
// call it and that the account service's server-list endpoint is its only caller. Both
// sentences are true today and neither is checked by anything, which is the shape this
// repository has decided not to leave alone: a guarantee no machine checks is either
// enforced or not asserted.
//
// Enforcing it is worth more here than the usual amount, because of what the mistake
// would cost and how quiet it would be. The world in a ticket is what stops one
// operator collecting its players' tickets and replaying them at somebody else's world,
// and it is what turns an account ticket away at a game server's door. A future caller
// — #102 wires ticket verification into the game server — that reaches for
// `VerifyAnyWorld` because the name is shorter, or because `Verify`'s world argument was
// inconvenient to obtain, compiles, passes every gate, and admits any account ticket to
// any world. Nothing about the resulting build looks wrong.
//
// Written in the shape internal/registry and internal/auth already use: an AST walk over
// the source rather than a question to the toolchain, so it is hermetic — no subprocess,
// no module cache, no network, and it works in a checkout that has never been built.
package ticket

import (
	"go/ast"
	"go/parser"
	"go/token"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// callersAllowed are the two directories, relative to server/, that may name
// [VerifyAnyWorld].
//
//   - internal/ticket, which defines and tests it.
//   - cmd/voxelheim-auth, whose server-list endpoint is the caller it exists for: that
//     endpoint has no world to compare against, because the list is what tells a player
//     which worlds there are.
//
// A third entry is a decision somebody makes here, in a diff a reviewer reads, rather
// than a call site nobody notices.
var callersAllowed = map[string]struct{}{
	"internal/ticket":    {},
	"cmd/voxelheim-auth": {},
}

func TestOnlyTheAccountServiceNamesVerifyAnyWorld(t *testing.T) {
	t.Parallel()

	root := filepath.Join("..", "..")
	if _, err := os.Stat(filepath.Join(root, "go.mod")); err != nil {
		t.Fatalf("the server workspace is not two directories up from this test: %v", err)
	}

	fset := token.NewFileSet()
	found := 0
	var offenders []string

	err := filepath.WalkDir(root, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() {
			if name := entry.Name(); name == "gen" || name == "testdata" {
				return fs.SkipDir
			}
			return nil
		}
		if !strings.HasSuffix(entry.Name(), ".go") {
			return nil
		}

		parsed, err := parser.ParseFile(fset, path, nil, 0)
		if err != nil {
			return err
		}

		dir, err := filepath.Rel(root, filepath.Dir(path))
		if err != nil {
			return err
		}
		dir = filepath.ToSlash(dir)

		ast.Inspect(parsed, func(n ast.Node) bool {
			id, ok := n.(*ast.Ident)
			if !ok || id.Name != "VerifyAnyWorld" {
				return true
			}
			found++
			if _, allowed := callersAllowed[dir]; !allowed {
				offenders = append(offenders, dir+"/"+entry.Name())
			}
			return true
		})
		return nil
	})
	if err != nil {
		t.Fatalf("walking the server workspace: %v", err)
	}

	// Fails closed, which is the half that is easy to leave out: a walk that matched
	// nothing at all would satisfy the check below perfectly while proving nothing —
	// including in the case that matters most, somebody renaming the function and
	// leaving this test pointing at a name that no longer exists.
	if found == 0 {
		t.Fatal("the walk found no reference to VerifyAnyWorld at all, not even its own definition")
	}
	if len(offenders) != 0 {
		t.Errorf("VerifyAnyWorld is named outside the account service, which drops the world comparison that keeps one world's tickets out of another: %v", offenders)
	}
}
