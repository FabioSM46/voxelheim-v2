package persist

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// ClockVersion is the on-disk format version of the clock file.
//
// Bump it for any change to the layout below, a purely additive one included: a reader
// of an older build must refuse a newer file rather than parse a prefix of it.
// Separate from [StoreVersion], [StructuresVersion] and world.StoreVersion, because a
// clock, a camp, a player record and a chunk delta change for unrelated reasons — and
// **world.StoreVersion in particular must not be reached for**, since bumping it
// invalidates every stored chunk delta in every existing world.
const ClockVersion uint32 = 1

// On-disk layout, little-endian throughout, one file for the whole world.
//
//	clock.bin
//	    magic[4] version:u32 tick_of_day:u32 crc32:u32
//
// Sixteen bytes, and the only file under the world directory whose size is fixed by the
// format rather than by its contents. That is worth one thing at the read: the size
// check below is an equality rather than a ceiling, so a truncated file and an
// over-long one are both refused before a byte is loaded, and neither can be read as a
// shorter or longer clock.
//
// **What is not here is the absolute tick.** The simulation counts ticks in a uint64
// that only ever increases, and storing that would mean a restarted world claiming an
// uptime it never had. What outlives the process is where the world stands in its day,
// which is the one number anything asks about — see game.IsNight.
//
// **Nor is the day length.** It is a constant of this build announced in every welcome,
// not a property of the world: recording it would create a second copy to keep in step
// with game.DayLengthTicks, and the only thing that copy could ever do is disagree.
// A build whose day length changed reads this file's tick against its own constant and
// refuses it if it no longer fits, which is exactly what the range check in
// game.Sim.RestoreClock is for.
const (
	clockFileName = "clock.bin"

	offClockTickOfDay = world.HeaderSize
	clockFileSize     = offClockTickOfDay + 4 + world.ChecksumSize
)

var clockMagic = [4]byte{'V', 'X', 'H', 'C'}

// ClockStore is one world's clock file.
//
// **A nil *ClockStore is the ephemeral world**, and every method is a no-op on one
// rather than a branch at each call site — the shape a nil [Store], a nil
// [StructureStore] and a nil world.Store all already have. An ephemeral world keeps a
// clock in memory and its night arrives exactly on time; what it does not do is
// remember which part of the day it was in, which is the difference the operator chose.
//
// Like [StructureStore] and unlike [Store], it owns exactly one file rewritten whole.
// That is what makes it safe with no lock of its own for the single writer it has: the
// autosave loop and the shutdown flush are ordered against each other by the worker
// wait group, never concurrent.
type ClockStore struct {
	path string
}

// OpenClockStore prepares the clock file under worldDir.
//
// It does not create the file: a world that has not been played in has no clock file,
// and that is the same fact as one reading zero rather than a state to initialise.
// worldDir has already been seed-checked by world.OpenStore, which runs first, so
// nothing here re-asks whether this directory belongs to this world.
func OpenClockStore(worldDir string) (*ClockStore, error) {
	if worldDir == "" {
		// Not a nil store returned quietly, for the reason [OpenStore] and
		// [OpenStructureStore] give: an empty -world-dir is the ephemeral world, and
		// choosing it is main's decision rather than a shape this constructor should
		// accept and forget about.
		return nil, errors.New("persist: the world directory must be named")
	}
	if err := os.MkdirAll(worldDir, 0o755); err != nil {
		return nil, fmt.Errorf("persist: creating %s: %w", worldDir, err)
	}

	// Whatever a crash left mid-rename, for the reason [OpenStructureStore] sweeps: this
	// store writes through world.WriteAtomic and inherits its leftovers. Inert either
	// way — a reader only ever opens the exact path below. One name, for the reason
	// given there: the directory is the operator's and only this file in it is ours
	// (#137).
	world.SweepTemporaries(worldDir, clockFileName)
	return &ClockStore{path: filepath.Join(worldDir, clockFileName)}, nil
}

// Path is the file this store writes. Empty for an ephemeral world.
func (s *ClockStore) Path() string {
	if s == nil {
		return ""
	}
	return s.path
}

// Load reads the tick of the day this world last wrote down.
//
// Three answers, the same three [Store.Load] and [StructureStore.Load] give: found,
// absent, or unreadable. A world nobody has played in has no file, which is not an
// error — it is a world that starts at first light. A file that exists and cannot be
// read **is** an error and must stay one: reporting it as "no clock" would restart the
// world at dawn and then write that over the only record of where its day had got to.
//
// **What comes back is the number that was stored and nothing more.** Whether it names
// a tick that can exist is decided by game.Sim.RestoreClock, which owns the day length;
// this package judges what a file can be wrong about and no further.
func (s *ClockStore) Load() (uint32, bool, error) {
	if s == nil {
		return 0, false, nil
	}

	info, err := os.Stat(s.path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return 0, false, nil
	case err != nil:
		return 0, false, fmt.Errorf("persist: reading %s: %w", s.path, err)
	}
	// Before the read, not after, for the reason every store here checks a size first:
	// a file that is not this size is not one this format wrote, and finding that out by
	// allocating it is how a corrupt directory becomes an out-of-memory.
	if info.Size() != clockFileSize {
		return 0, false, fmt.Errorf("%w: %s is %d bytes, and a clock file is exactly %d",
			world.ErrCorruptStore, s.path, info.Size(), clockFileSize)
	}

	data, err := os.ReadFile(s.path)
	if err != nil {
		return 0, false, fmt.Errorf("persist: reading %s: %w", s.path, err)
	}

	tickOfDay, err := decodeClock(data)
	if err != nil {
		return 0, false, fmt.Errorf("%s: %w", s.path, err)
	}
	return tickOfDay, true, nil
}

// Save writes the world's time of day, atomically. A no-op in an ephemeral world.
//
// **Every uint32 it can write it can read back**, so unlike [StructureStore.Save] there
// is nothing for this to refuse. The cap that one enforces on both sides exists because
// the format has a limit of its own; this format has none, and the only value that
// could be wrong here is one the simulation would have to have produced — which is a
// bug in game, caught by game's own invariant, and not something a second opinion on
// this side would improve.
func (s *ClockStore) Save(tickOfDay uint32) error {
	if s == nil {
		return nil
	}
	return world.WriteAtomic(s.path, encodeClock(tickOfDay))
}

// encodeClock lays out the sixteen bytes.
func encodeClock(tickOfDay uint32) []byte {
	buf := world.NewRecord(offClockTickOfDay, 4, clockMagic, ClockVersion)
	binary.LittleEndian.PutUint32(buf[offClockTickOfDay:offClockTickOfDay+4], tickOfDay)
	world.PutChecksum(buf)
	return buf
}

// decodeClock parses the clock, refusing anything it cannot read exactly.
//
// Its own length check rather than a reliance on [ClockStore.Load]'s: the size test up
// there guards the allocation, this one guards the indexing, and a decoder that is only
// safe because of its caller is one edit away from not being.
func decodeClock(data []byte) (uint32, error) {
	if len(data) != clockFileSize {
		return 0, fmt.Errorf("%w: %d bytes is not the %d a clock file has",
			world.ErrCorruptStore, len(data), clockFileSize)
	}
	if err := world.CheckHeader(data, clockMagic, ClockVersion); err != nil {
		return 0, err
	}
	if err := world.CheckChecksum(data); err != nil {
		return 0, err
	}
	return binary.LittleEndian.Uint32(data[offClockTickOfDay : offClockTickOfDay+4]), nil
}
