package persist

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
)

const playerMigrationMarker = "players.migration"

var ErrPlayerMigrationInterrupted = errors.New("persist: interrupted player migration requires operator recovery; preserve original and staged records")

// Presence alone refuses startup, even for a damaged marker or one left after a
// completed installation. This is deliberately fail-closed, not automatic recovery:
// the operator must inspect the original/staging directories before removing it.
// Check BEFORE MkdirAll, sweeping temporaries or indexing anything. A process can
// stop between the two directory renames, when players does not exist at all.
func refuseInterruptedPlayerMigration(dir string) error {
	_, err := os.Lstat(filepath.Join(dir, playerMigrationMarker))
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("inspecting player migration marker: %w", err)
	}
	return ErrPlayerMigrationInterrupted
}
