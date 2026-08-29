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
const ClockVersion uint32 = 2

// previousClockVersion is the sixteen-byte clock this store wrote before the absolute
// tick existed: magic, version, tick_of_day, crc32 and nothing else. Read, migrated and
// never written — see [decodeV1Clock].
const previousClockVersion uint32 = 1

// On-disk layout, little-endian throughout, one file for the whole world.
//
//	clock.bin
//	    magic[4] version:u32 tick_of_day:u32 world_tick:u64 next_storm_unix:i64 crc32:u32
//
// Thirty-two bytes, and the only file under the world directory whose size is fixed by
// the format rather than by its contents. That is worth two things at the read: the size
// picks the layout, so a version-1 file is recognised before a byte of it is
// interpreted; and within each layout the check is an equality rather than a ceiling, so
// a truncated file and an over-long one are both refused before a byte is loaded.
//
// **The absolute tick is here, and it did not used to be.** This block used to say that
// storing it would mean a restarted world claiming an uptime it never had. Two features
// then needed a clock that survives a restart — weather that drifts across days, and a
// storm counted in weeks — and neither can be built on a number that recurs every twenty
// minutes. The uptime worry was answering the wrong question: world_tick is the *world's*
// time, not the process's. A world that ran for a day and was switched off for a month
// comes back having lived one day, which is exactly what it did.
//
// **next_storm_unix is a wall-clock second, the one field here that is not a tick.** The
// storm rides a real week (GDD §9), and a week that includes the days a server spent
// switched off is not a quantity any tick counter can hold. Zero means unscheduled — see
// game.Sim.NextStorm for why an absent deadline needs no flag beside it.
//
// **Nor is the day length here.** It is a constant of this build announced in every
// welcome, not a property of the world: recording it would create a second copy to keep
// in step with game.DayLengthTicks, and the only thing that copy could ever do is
// disagree. A build whose day length changed reads this file's ticks against its own
// constant and refuses them if they no longer fit, which is what the checks in
// game.Sim.RestoreClock are for.
const (
	clockFileName = "clock.bin"

	offClockTickOfDay = world.HeaderSize
	offClockWorldTick = offClockTickOfDay + 4
	offClockNextStorm = offClockWorldTick + 8
	clockFileSize     = offClockNextStorm + 8 + world.ChecksumSize

	// clockV1FileSize is the whole of the previous format: the same header and tick of
	// day, and then the checksum.
	clockV1FileSize = offClockTickOfDay + 4 + world.ChecksumSize
)

var clockMagic = [4]byte{'V', 'X', 'H', 'C'}

// Clock is what one clock file holds: where the world stands in its day, how many ticks
// it has ever run, and when its next storm falls due.
//
// A struct on the way out and three arguments on the way in, which is less inconsistent
// than it looks: [ClockStore.Save] is handed three values a caller captured from the
// simulation in one lock, and separate parameters are what make a transposed pair a
// compile error. A read has no such pairing to get wrong and would otherwise be five
// return values.
type Clock struct {
	TickOfDay     uint32
	WorldTick     uint64
	NextStormUnix int64
}

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

// Load reads the clock this world last wrote down.
//
// Three answers, the same three [Store.Load] and [StructureStore.Load] give: found,
// absent, or unreadable. A world nobody has played in has no file, which is not an
// error — it is a world that starts at first light. A file that exists and cannot be
// read **is** an error and must stay one: reporting it as "no clock" would restart the
// world at dawn and then write that over the only record of where its day had got to.
//
// **A version-1 file is found rather than refused**, and comes back as the world it
// describes — see [decodeV1Clock]. It is rewritten in this build's format by the first
// save, which the autosave loop makes within one interval of the start.
//
// **What comes back is what was stored and nothing more.** Whether it names a clock that
// can exist is decided by game.Sim.RestoreClock, which owns the day length; this package
// judges what a file can be wrong about and no further.
func (s *ClockStore) Load() (Clock, bool, error) {
	if s == nil {
		return Clock{}, false, nil
	}

	info, err := os.Stat(s.path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return Clock{}, false, nil
	case err != nil:
		return Clock{}, false, fmt.Errorf("persist: reading %s: %w", s.path, err)
	}
	// Before the read, not after, for the reason every store here checks a size first:
	// a file that is not one of these sizes is not one this format ever wrote, and
	// finding that out by allocating it is how a corrupt directory becomes an
	// out-of-memory.
	if info.Size() != clockFileSize && info.Size() != clockV1FileSize {
		return Clock{}, false, fmt.Errorf("%w: %s is %d bytes, and a clock file is exactly %d, or the %d version %d wrote",
			world.ErrCorruptStore, s.path, info.Size(), clockFileSize, clockV1FileSize, previousClockVersion)
	}

	data, err := os.ReadFile(s.path)
	if err != nil {
		return Clock{}, false, fmt.Errorf("persist: reading %s: %w", s.path, err)
	}

	clock, err := decodeClock(data)
	if err != nil {
		return Clock{}, false, fmt.Errorf("%s: %w", s.path, err)
	}
	return clock, true, nil
}

// Save writes the world's clock, atomically, in this build's format. A no-op in an
// ephemeral world.
//
// **Every value it can write it can read back**, so unlike [StructureStore.Save] there
// is nothing for this to refuse. The cap that one enforces on both sides exists because
// the format has a limit of its own; this format has none, and the only values that
// could be wrong here are ones the simulation would have to have produced — a bug in
// game, caught by game's own invariant.
//
// **Three arguments rather than a [Clock]**: the pair this writes down has to have been
// captured together — see game.Sim.Clock — and separate parameters are what make the
// call site say which value is which.
func (s *ClockStore) Save(tickOfDay uint32, worldTick uint64, nextStormUnix int64) error {
	if s == nil {
		return nil
	}
	return world.WriteAtomic(s.path, encodeClock(tickOfDay, worldTick, nextStormUnix))
}

// encodeClock lays out the thirty-two bytes.
func encodeClock(tickOfDay uint32, worldTick uint64, nextStormUnix int64) []byte {
	buf := world.NewRecord(offClockTickOfDay, 4+8+8, clockMagic, ClockVersion)
	binary.LittleEndian.PutUint32(buf[offClockTickOfDay:offClockTickOfDay+4], tickOfDay)
	binary.LittleEndian.PutUint64(buf[offClockWorldTick:offClockWorldTick+8], worldTick)
	// Two's-complement in both directions, undone exactly by the decoder, so the field
	// carries the whole of an int64 rather than half of one.
	binary.LittleEndian.PutUint64(buf[offClockNextStorm:offClockNextStorm+8], uint64(nextStormUnix))
	world.PutChecksum(buf)
	return buf
}

// decodeClock parses the clock, refusing anything it cannot read exactly.
//
// Its own length check rather than a reliance on [ClockStore.Load]'s: the size test up
// there guards the allocation, this one guards the indexing, and a decoder that is only
// safe because of its caller is one edit away from not being.
//
// **The length picks the layout and the header still decides.** Both formats are fixed
// sizes and they differ, so a file of either length is read as that format and then has
// its declared version checked against the one that length belongs to. A version-2 file
// truncated to sixteen bytes fails on its version rather than being read as a version-1
// clock, and a version-1 file padded to thirty-two fails the same way.
func decodeClock(data []byte) (Clock, error) {
	switch len(data) {
	case clockFileSize:
		return decodeV2Clock(data)
	case clockV1FileSize:
		return decodeV1Clock(data)
	default:
		return Clock{}, fmt.Errorf("%w: %d bytes is neither the %d a clock file has nor the %d version %d wrote",
			world.ErrCorruptStore, len(data), clockFileSize, clockV1FileSize, previousClockVersion)
	}
}

func decodeV2Clock(data []byte) (Clock, error) {
	if err := world.CheckHeader(data, clockMagic, ClockVersion); err != nil {
		return Clock{}, err
	}
	if err := world.CheckChecksum(data); err != nil {
		return Clock{}, err
	}
	return Clock{
		TickOfDay:     binary.LittleEndian.Uint32(data[offClockTickOfDay : offClockTickOfDay+4]),
		WorldTick:     binary.LittleEndian.Uint64(data[offClockWorldTick : offClockWorldTick+8]),
		NextStormUnix: int64(binary.LittleEndian.Uint64(data[offClockNextStorm : offClockNextStorm+8])),
	}, nil
}

// decodeV1Clock reads the sixteen-byte clock and says what it means in this format.
//
// **A migration and not a refusal**, the choice decodeV7Record makes for a player
// record. Refusing would discard a working world's day phase over a field that did not
// exist when it was written.
//
// world_tick becomes the stored tick of day: the only value that satisfies the invariant
// game.Sim.RestoreClock enforces without claiming history the file does not record. The
// world comes back inside its first day, at the phase it actually stopped at. Nothing in
// sixteen bytes could say how long it really ran, and the honest version of that is an
// absolute clock that starts counting now.
//
// next_storm_unix becomes zero, which is "unscheduled": a file written before storms
// existed says nothing about one.
func decodeV1Clock(data []byte) (Clock, error) {
	if err := world.CheckHeader(data, clockMagic, previousClockVersion); err != nil {
		return Clock{}, err
	}
	if err := world.CheckChecksum(data); err != nil {
		return Clock{}, err
	}
	tickOfDay := binary.LittleEndian.Uint32(data[offClockTickOfDay : offClockTickOfDay+4])
	return Clock{TickOfDay: tickOfDay, WorldTick: uint64(tickOfDay)}, nil
}
