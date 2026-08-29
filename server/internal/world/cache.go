package world

import (
	"container/list"
	"context"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
)

// DefaultCacheCapacity is how many chunks a cache keeps before evicting the least
// recently used.
//
// A chunk is 64 KiB of blocks plus its encoded payload, so 1024 of them is on the
// order of 70 MiB — comfortably more than the (2·3+1)³ = 343 chunks one session's
// view distance needs, and bounded, which is the point. Eviction is safe precisely
// because generation is deterministic and the edits on top of it are recorded
// separately: a chunk thrown away regenerates to the same bytes and is composed with
// the same deltas.
const DefaultCacheCapacity = 1024

// DefaultWorkers is how many chunks may be generated concurrently.
const DefaultWorkers = 4

// Cache generates chunks on demand, composes the world's edits onto them, and
// remembers the result — so a chunk is generated once however many sessions ask for
// it.
//
// Two bounds matter here. Concurrency is capped, so a player who teleports cannot
// hand the machine 343 simultaneous generations; and residency is capped, so a server
// running for a week does not accumulate every chunk anyone has ever seen.
//
// # What a cached chunk is
//
// **Generated base plus deltas.** Generate is a pure function of (seed, coord) and
// stays one; every edit lives in the Deltas layer, and a chunk handed to any consumer
// is the base with that layer applied. Nothing bakes an edit into the generator,
// because the Fimbulvetr storm has to be able to throw the deltas away and get the
// original world back.
//
// # The locking rule
//
// A composed chunk is **never mutated after it is published**. An edit builds a patched
// copy and swaps the pointer, so every reader — collision on the tick goroutine most of
// all — reads a chunk nothing can write to, with no lock and no atomic per voxel.
//
// Three locks, in this order where they nest, and none of them is ever held across
// Generate:
//
//	composeMu  serialises composition: recording an edit, patching the resident chunk,
//	           re-encoding it. It is what keeps the delta layer and every resident
//	           chunk from ever disagreeing.
//	mu         guards the entry map and the LRU list, and nothing else.
//	Deltas.mu  guards the delta map, so Deltas is also safe used on its own.
//	saveMu     serialises saves against each other. It nests under none of the above:
//	           a save takes neither composeMu nor mu, which is what keeps disk I/O off
//	           the tick loop's and the streamer's paths. See store.go.
//	dirtyMu    guards the set of chunks awaiting a write, and is never held across one.
type Cache struct {
	seed     int64
	capacity int
	slots    chan struct{}

	// deltas outlives every entry deliberately: a chunk evicted and regenerated has to
	// come back *with* its edits, which only works while the record of them is not part
	// of what eviction discards.
	deltas *Deltas

	// revision counts accepted edits. It exists for consumers that remember a chunk
	// pointer across calls — collision does — because a published chunk stays readable
	// for ever but stops being current the moment somebody digs into it. An atomic
	// counter is how they notice without taking mu per voxel.
	revision atomic.Uint64

	composeMu sync.Mutex

	mu      sync.Mutex
	entries map[Coord]*list.Element
	lru     *list.List

	// The persistence half, declared here with the rest of the state and implemented in
	// store.go — a nil store is an ephemeral world, and every method below is a no-op
	// against one. Only deltas ever reach the disk: the generated base stays a function
	// of the seed.
	store   *Store
	saveMu  sync.Mutex // serialises Flush against Flush, so no stale snapshot lands late
	dirtyMu sync.Mutex
	dirty   map[Coord]struct{}
}

// composition is a composed chunk and its encoded payload, published as one value.
//
// The pair travels together so a reader cannot get a chunk from before an edit beside
// the payload from after it. Both fields are read-only once published.
type composition struct {
	chunk   *Chunk
	encoded []uint16
}

type cacheEntry struct {
	coord Coord
	ready chan struct{}
	err   error

	// composed is written by the goroutine that generates this chunk and replaced by
	// every edit that patches it, always under Cache.composeMu. Readers take nothing:
	// an atomic load hands them a chunk that will never change.
	composed atomic.Pointer[composition]
}

// NewCache returns an ephemeral cache for one world seed: edits live for as long as the
// process does. Zero or negative arguments fall back to the defaults.
func NewCache(seed int64, workers, capacity int) *Cache {
	return newCache(seed, nil, workers, capacity)
}

// NewPersistentCache returns a cache whose edits are loaded from store as chunks are
// first composed, and written back to it by Flush and SaveLoop.
//
// The seed comes from the store rather than from the caller, and that is deliberate:
// OpenStore has already refused a directory recorded under a different one, so there is
// exactly one seed in play and no second place for it to disagree.
//
// store must be non-nil; NewCache is the ephemeral world.
func NewPersistentCache(store *Store, workers, capacity int) *Cache {
	return newCache(store.Seed(), store, workers, capacity)
}

func newCache(seed int64, store *Store, workers, capacity int) *Cache {
	if workers <= 0 {
		workers = DefaultWorkers
	}
	if capacity <= 0 {
		capacity = DefaultCacheCapacity
	}

	return &Cache{
		seed:     seed,
		capacity: capacity,
		slots:    make(chan struct{}, workers),
		deltas:   NewDeltas(),
		entries:  make(map[Coord]*list.Element),
		lru:      list.New(),
		store:    store,
		dirty:    make(map[Coord]struct{}),
	}
}

// Seed is the world seed every chunk in this cache is generated from.
func (c *Cache) Seed() int64 { return c.seed }

// Len is how many chunks are resident.
func (c *Cache) Len() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.lru.Len()
}

// Revision counts the edits this cache has applied.
//
// The invalidation signal for anything that holds on to a chunk pointer. It is bumped
// *after* the patched chunk is published, so a consumer that reads a new revision and
// then re-reads the chunk can never be handed the state from before the edit.
func (c *Cache) Revision() uint64 { return c.revision.Load() }

// Get returns the chunk at coord and its encoded payload, generating it if this is
// the first request.
//
// Concurrent callers for the same coordinate wait for one generation rather than
// racing to do the same work — the reason an entry is published to the map *before*
// the chunk exists, with a channel that says when it does.
//
// Generation runs outside the lock and inside a bounded semaphore, so a slow chunk
// never blocks the whole cache and a burst never oversubscribes the machine.
func (c *Cache) Get(ctx context.Context, coord Coord) (*Chunk, []uint16, error) {
	entry, mine := c.claim(coord)
	if !mine {
		select {
		case <-entry.ready:
			if entry.err != nil {
				return nil, nil, entry.err
			}
			composed := entry.composed.Load()
			return composed.chunk, composed.encoded, nil
		case <-ctx.Done():
			return nil, nil, fmt.Errorf("world: waiting for chunk %+v: %w", coord, ctx.Err())
		}
	}

	// This goroutine owns the generation, so it also owns finishing it: every exit
	// path below closes ready, or a waiter would block forever.
	if err := c.acquire(ctx); err != nil {
		entry.err = fmt.Errorf("world: generating chunk %+v: %w", coord, err)
		close(entry.ready)
		c.forget(coord, entry)
		return nil, nil, entry.err
	}
	defer func() { <-c.slots }()

	// The stored edits first, because this is the fallible step and the cheap one: a
	// world directory that cannot be read must not cost a generation, and a chunk must
	// never be composed from the base alone when a file says the base is not the whole
	// truth. A refusal here is what keeps a corrupt file from being served as terrain —
	// the one outcome that would quietly overwrite what a player built.
	if err := c.hydrate(coord); err != nil {
		entry.err = fmt.Errorf("world: loading the stored edits for chunk %+v: %w", coord, err)
		close(entry.ready)
		c.forget(coord, entry)
		return nil, nil, entry.err
	}

	// Generation next and outside every lock — it is the millisecond-scale part — then
	// composition under composeMu. Publishing before ready closes is what lets Apply
	// treat "the composition is not there yet" as "the generator will compose my delta".
	composed := c.compose(entry, Generate(c.seed, coord))
	close(entry.ready)

	return composed.chunk, composed.encoded, nil
}

// compose applies the edit layer to a freshly generated base and publishes the result.
//
// Under composeMu, and that is the entire correctness argument for edits that land
// while a chunk is being generated. There are exactly two orders. Either the edit
// records its delta first, and this call composes it in; or this call publishes first,
// and the edit finds a composition to patch. Apply takes the same lock around *both*
// of its steps, so no third interleaving exists and no accepted edit can go missing
// from a resident chunk.
//
// base must not be visible to anyone else yet: ApplyTo writes voxels, and a published
// chunk is read without a lock.
func (c *Cache) compose(entry *cacheEntry, base *Chunk) *composition {
	c.composeMu.Lock()
	defer c.composeMu.Unlock()

	c.deltas.ApplyTo(base)
	composed := &composition{chunk: base, encoded: Encode(base)}
	entry.composed.Store(composed)
	return composed
}

// claim returns the entry for coord, and whether this caller is the one that must
// generate it.
func (c *Cache) claim(coord Coord) (*cacheEntry, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if element, ok := c.entries[coord]; ok {
		c.lru.MoveToFront(element)
		return element.Value.(*cacheEntry), false
	}

	entry := &cacheEntry{coord: coord, ready: make(chan struct{})}
	c.entries[coord] = c.lru.PushFront(entry)
	c.evictLocked()
	return entry, true
}

// resident returns the entry currently holding coord, or nil.
//
// It deliberately does not touch the LRU order: this is the edit path's lookup, and an
// edit is a write to a chunk rather than a read of one. Get has already moved the entry
// to the front a moment earlier anyway.
func (c *Cache) resident(coord Coord) *cacheEntry {
	c.mu.Lock()
	defer c.mu.Unlock()

	if element, ok := c.entries[coord]; ok {
		return element.Value.(*cacheEntry)
	}
	return nil
}

// forget drops a failed entry so a later Get retries instead of replaying the
// failure forever. It only removes the entry it was given: a retry that already
// replaced it must not be evicted by this one's cleanup.
func (c *Cache) forget(coord Coord, entry *cacheEntry) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if element, ok := c.entries[coord]; ok && element.Value.(*cacheEntry) == entry {
		c.lru.Remove(element)
		delete(c.entries, coord)
	}
}

func (c *Cache) acquire(ctx context.Context) error {
	// The explicit check is load-bearing: a select whose cases are both ready picks
	// one at random, so with a free slot and an already-cancelled context this would
	// generate the chunk anyway — about half the time. Cancellation has to win
	// deterministically, or the bug shows up months later as a flaky test.
	if err := ctx.Err(); err != nil {
		return err
	}

	select {
	case c.slots <- struct{}{}:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// evictLocked trims the cache to capacity. Entries are evicted from the back, and
// a generation in flight was just pushed to the front, so eviction does not
// discard work that is still being produced.
func (c *Cache) evictLocked() {
	for c.lru.Len() > c.capacity {
		oldest := c.lru.Back()
		if oldest == nil {
			return
		}
		c.lru.Remove(oldest)
		delete(c.entries, oldest.Value.(*cacheEntry).coord)
	}
}

// ErrNotResident reports that a chunk is not in the cache. Callers that must not
// trigger generation — anything running on the tick goroutine — use Peek and treat
// this as "ask again next tick".
var ErrNotResident = errors.New("world: chunk is not resident")

// Peek returns a chunk only if it is already generated.
//
// The tick loop must never generate terrain: a tick that waits on a chunk is a
// tick that misses its deadline for every connected player. Peek is how the
// simulation reads the world without ever paying that cost.
//
// The chunk it returns carries every edit accepted before the call. It will not carry
// one accepted after it — an edit publishes a *new* chunk rather than writing to this
// one — so a caller that keeps the pointer must watch Revision to know when to ask
// again.
func (c *Cache) Peek(coord Coord) (*Chunk, error) {
	c.mu.Lock()
	element, ok := c.entries[coord]
	if ok {
		c.lru.MoveToFront(element)
	}
	c.mu.Unlock()

	if !ok {
		return nil, fmt.Errorf("%w: %+v", ErrNotResident, coord)
	}

	entry := element.Value.(*cacheEntry)
	select {
	case <-entry.ready:
		if entry.err != nil {
			return nil, entry.err
		}
		composed := entry.composed.Load()
		if composed == nil {
			// Unreachable: compose publishes before ready closes. Stated rather than
			// dereferenced, because the alternative to an error here is a nil map read on
			// the tick goroutine.
			return nil, fmt.Errorf("%w: %+v has no composition", ErrNotResident, coord)
		}
		return composed.chunk, nil
	default:
		return nil, fmt.Errorf("%w: %+v is still being generated", ErrNotResident, coord)
	}
}

// BlockAt returns the block at a world voxel, generating its chunk if this is the
// first time anyone has asked for it.
//
// The blocking counterpart of Peek, and it must never be called from the tick
// goroutine for exactly the reason Peek exists. Resolving an edit needs a definite
// answer about the target voxel — "not loaded yet" is not one of the possible replies —
// and a session goroutine is allowed to wait for it.
//
// The returned block is meaningless when err is non-nil; there is no fail-safe default
// here, because "solid" fails closed for a break and open for a place.
func (c *Cache) BlockAt(ctx context.Context, x, y, z int64) (Block, error) {
	chunk, _, err := c.Get(ctx, ChunkOf(x, y, z))
	if err != nil {
		return Air, err
	}
	return chunk.At(Local(x), Local(y), Local(z)), nil
}

// Apply changes one world voxel, if allow accepts the block that is there at the moment
// of the write, and makes the change visible to everything that reads this cache.
//
// # allow is where legality belongs
//
// The caller's rule — a break needs something to break, a placement needs room — runs
// **inside the same critical section as the write**. Reading the voxel first and writing
// afterwards would leave a window in which two players edit the same voxel, are both told
// they succeeded, and broadcast two different answers while the delta layer keeps only
// one of them. Its error is returned unchanged, so the caller can log the refusal it
// wrote.
//
// It must be pure and must not block: composeMu is held, and every chunk being composed
// anywhere in the server is waiting behind it.
//
// # The order of the steps is the correctness argument
//
//  1. **Generate first, outside every lock.** Get can block for milliseconds, and an
//     edit holding composeMu across it would stall every other edit and every chunk
//     being composed. A voxel cannot be edited without knowing which chunk holds it
//     anyway.
//  2. **Read the resident composition and ask allow**, under composeMu, so the block the
//     rule judged is the block the write replaces.
//  3. **Record the delta.** The record is what survives eviction: a chunk discarded after
//     it regenerates *with* the edit, so the worst case is composing it twice, which is
//     the same value.
//  4. **Replace the resident chunk with a patched copy, re-encoded.** The copy is what
//     lets collision read voxels with no lock at all, and the fresh payload is what
//     makes a player who joins afterwards receive the modified chunk rather than the
//     generated one.
//  5. **Bump the revision, last.** A consumer that memoises a chunk and watches this
//     counter must never see the new number beside the old chunk.
//  6. **Mark the chunk for the next save.** A mark, not a write: the disk is nowhere near
//     this path. See store.go for what the saver does with it.
func (c *Cache) Apply(ctx context.Context, x, y, z int64, block Block, allow func(current Block) error) error {
	return c.apply(ctx, x, y, z, block, nil, allow)
}

// ApplyGuarded is Apply with one caller-owned guard acquired after chunk
// generation and before the voxel write begins.
//
// guard runs outside every cache lock. It exists for state that must remain
// consistent with the write without being held across Get: Player.Edit uses it
// to acquire one player's inventory lock. A guard may deliberately keep that
// lock held after it returns nil; its caller must release it after ApplyGuarded
// returns, on both success and error. A guard returning an error must unwind any
// state it acquired itself. If generation fails, guard is never called. allow
// retains Apply's stricter contract and runs under composeMu.
func (c *Cache) ApplyGuarded(ctx context.Context, x, y, z int64, block Block, guard func() error, allow func(current Block) error) error {
	return c.apply(ctx, x, y, z, block, guard, allow)
}

func (c *Cache) apply(ctx context.Context, x, y, z int64, block Block, guard func() error, allow func(current Block) error) error {
	coord := ChunkOf(x, y, z)
	if _, _, err := c.Get(ctx, coord); err != nil {
		return fmt.Errorf("world: editing voxel %d,%d,%d: %w", x, y, z, err)
	}
	if guard != nil {
		if err := guard(); err != nil {
			return err
		}
	}
	index := Index(Local(x), Local(y), Local(z))

	c.composeMu.Lock()
	defer c.composeMu.Unlock()

	// The chunk this edit was checked against has to be the chunk it is written to, so
	// both come from the composition that is resident *now*. A chunk evicted or being
	// regenerated since the Get above leaves nothing to check against, and guessing is
	// not one of the options: the edit is refused and the client may ask again. Reaching
	// for Get here instead would generate a chunk while holding composeMu, which is the
	// one thing this path may not do.
	entry := c.resident(coord)
	if entry == nil {
		return fmt.Errorf("%w: %+v was evicted while its edit was being applied", ErrNotResident, coord)
	}
	current := entry.composed.Load()
	if current == nil {
		return fmt.Errorf("%w: %+v is being regenerated", ErrNotResident, coord)
	}

	if allow != nil {
		if err := allow(current.chunk.Blocks[index]); err != nil {
			return err
		}
	}

	c.deltas.Record(coord, index, block)

	patched := current.chunk.Clone()
	patched.Blocks[index] = block
	// Re-encoded here rather than invalidated and rebuilt on demand. Lazy would save the
	// work when nobody joins, and would cost a second race: whichever reader found the
	// nil would have to rebuild it, under this same lock, with no way to tell a stale
	// payload from one being built.
	entry.composed.Store(&composition{chunk: patched, encoded: Encode(patched)})

	c.revision.Add(1)
	// A map insert under a mutex nothing holds across I/O. Inside composeMu rather than
	// after it so an accepted edit cannot be recorded and then lose its mark to a badly
	// timed shutdown; the cost is that every chunk being composed waits behind a map
	// insert, which is the same order as the delta Record two statements above.
	c.markDirty(coord)
	return nil
}

// Regenerate puts one chunk back the way the seed made it: its edits are forgotten in
// memory and removed from disk, and if anybody is holding it, the freshly generated base
// is published in place of the composition that carried them.
//
// **This is the Fimbulvetr's primitive.** It is a removal rather than a rewrite for the
// reason store.go opens with — Generate is a pure function of (seed, coord) and only the
// edits are ever recorded — so "the world the way it was" is simply what is left once
// they are gone. Nothing here diffs two worlds, and nothing here consults the generator
// for a chunk nobody is looking at.
//
// # The order of the steps is the correctness argument
//
//  1. **The base is generated outside every lock**, and only when the chunk is resident.
//     Generate is the millisecond-scale part and no lock in this file is ever held across
//     it — saveMu least of all, because a save waiting behind it would be waiting for
//     arithmetic. It is safe out there because the base is a pure function of (seed,
//     coord) and so cannot go stale while the locks below are taken, and because the entry
//     it was generated for is read again under composeMu before anything is published into
//     it. A chunk nobody holds needs no base at all, because the next [Cache.Get] will
//     make one from a delta layer this call has already emptied.
//  2. **Serialised against a save**, through saveMu, from the Forget to the file removal.
//     Flush takes a coordinate out of the dirty set and only then reads its edits, so a
//     save running beside this one could be holding a snapshot of the very edits being
//     thrown away and would write the file back moments after it was removed. That is why
//     the removal in step 5 is inside this lock rather than after it.
//  3. **Forgotten under composeMu**, beside the publish and the dirty clear. That is the
//     same critical section [Cache.apply] uses, so there is no interleaving in which a
//     reader sees the deltas gone and the composed chunk still carrying them, and none in
//     which a save is scheduled for edits that no longer exist.
//  4. **The revision is bumped after the publish**, for [Cache.Revision]'s contract: a
//     consumer that memoises a chunk pointer must never read the new number beside the old
//     chunk. Collision reads a regenerated chunk exactly the way it reads an edited one.
//  5. **The file is removed last**, outside composeMu and still under saveMu, because it
//     is the I/O. Outside composeMu so no reader and no edit ever waits on a disk write;
//     under saveMu because step 2 is the whole reason the removal cannot be moved past it
//     — a Flush let through here is one holding a snapshot that would put the file back.
//     A failure there is returned and nothing is rolled back: the chunk is already
//     pristine in memory, and the file that outlived it is one the next call can remove
//     again.
//
// # What it does not promise
//
// A chunk being generated by another goroutine at this exact moment may have read its file
// in [Cache.hydrate] just before this call removed it, and may install those edits into the
// delta layer just after the Forget above — leaving one chunk still holding edits whose
// file is gone. It is the orphaned-generation window Restore's comment already describes,
// seen from the other side, and it is bounded in the same way: the edits are no longer
// stored, so the chunk is pristine again at the next restart or the next storm, and
// nothing is corrupted in between. Closing it properly would mean holding composeMu across
// a file read, which is the one thing this file's locking rule forbids.
func (c *Cache) Regenerate(coord Coord) error {
	// Before saveMu rather than inside it, so a save never waits on a generation. The
	// probe is a hint either way — it is not composeMu, so the entry can be evicted or
	// replaced before the publish — and the read under composeMu below is what decides.
	entry := c.resident(coord)
	var regenerated *composition
	if entry != nil {
		base := Generate(c.seed, coord)
		regenerated = &composition{chunk: base, encoded: Encode(base)}
	}

	c.saveMu.Lock()
	defer c.saveMu.Unlock()

	c.composeMu.Lock()
	c.deltas.Forget(coord)
	if regenerated != nil {
		// Read again under composeMu, because the probe above was not. An eviction or a
		// failed generation since then leaves a different entry or none at all, and a
		// composition published into either is one no reader ever waited for. An entry
		// that has not composed yet is left alone too: its own compose runs under this
		// lock and will find the delta layer already empty, which produces the same
		// pristine chunk without a second generation.
		if live := c.resident(coord); live == entry && entry.composed.Load() != nil {
			entry.composed.Store(regenerated)
			c.revision.Add(1)
		}
	}
	c.clearDirty(coord)
	c.composeMu.Unlock()

	if c.store == nil {
		return nil
	}
	return c.store.RemoveChunk(coord)
}
