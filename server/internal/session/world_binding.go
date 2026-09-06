package session

import (
	"context"
	"errors"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// WorldBinding is a borrowed world and its authoritative arrival position.
// The entry-policy caller must retain instance membership for the entire binding
// lifetime. Release the previous membership only after a successful transfer;
// retain the destination membership until departure or the session ends.
// Context cancellation ends the connection; it never silently returns to another
// world. No portal policy, occupancy or persistence is implemented here.
type WorldBinding struct {
	Chunks  *world.Cache
	Sim     *game.Sim
	Context context.Context
	Spawn   [3]float32
	// Arrival is an optional authoritative transition frame. It is written after
	// old-world output has drained and before any new-world output is enabled.
	Arrival []byte
}

type worldChange struct {
	binding WorldBinding
	result  chan error
}
type worldControl struct {
	changes chan worldChange
	done    chan struct{}
}

// ChangeWorld asks the session goroutine to change its binding. Once accepted,
// it waits for a definite result even if ctx ends, so the caller knows which
// world's membership it must retain. Calls from game callbacks are forbidden:
// the session must acquire the simulation lock to finish the transfer.
// Never call this from Serve's inbound handler: that goroutine uses its local
// changeWorld function directly rather than queuing a request to itself.
func (r *Registry) ChangeWorld(ctx context.Context, id uint64, binding WorldBinding) error {
	if binding.Chunks == nil || binding.Sim == nil || binding.Context == nil {
		return errors.New("session: incomplete world binding")
	}
	if err := binding.Context.Err(); err != nil {
		return err
	}
	r.mu.Lock()
	control := r.controls[id]
	r.mu.Unlock()
	if control == nil {
		return errors.New("session: character is not in a world")
	}
	binding.Arrival = append([]byte(nil), binding.Arrival...)
	request := worldChange{binding: binding, result: make(chan error, 1)}
	select {
	case control.changes <- request:
	case <-ctx.Done():
		return ctx.Err()
	case <-control.done:
		return errors.New("session: character disconnected")
	}
	select {
	case err := <-request.result:
		return err
	case <-control.done:
		return errors.New("session: character disconnected")
	}
}

// ForWorld returns the broadcast registry belonging to this cache. A world
// broadcast has no parameter it can forget to filter: its receiver owns only
// that world's subscriptions. The first world is the root's default scope for
// existing single-world callers; the server registers the open world first.
// Connection admission and the entity allocator remain on the root registry.
func (r *Registry) ForWorld(chunks *world.Cache) *Registry {
	if r.root != nil {
		return r.root.ForWorld(chunks)
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.worlds == nil {
		r.worlds = make(map[*world.Cache]*Registry)
	}
	if scope := r.worlds[chunks]; scope != nil {
		return scope
	}
	if len(r.worlds) == 0 {
		r.worlds[chunks] = r
		return r
	}
	scope := NewRegistry(r.limit)
	scope.root = r
	r.worlds[chunks] = scope
	return scope
}

func (v *View) clear() []world.Coord {
	v.mu.Lock()
	defer v.mu.Unlock()
	held := make([]world.Coord, 0, len(v.loaded))
	for coord := range v.loaded {
		held = append(held, coord)
	}
	clear(v.loaded)
	v.placed = false
	return held
}

// acquireWorld pins a scope while a session can use it. Releasing the final
// instance binding removes the cache key too, so the connection registry does
// not keep an expired instance's terrain alive indefinitely.
func (r *Registry) acquireWorld(chunks *world.Cache) (*Registry, func()) {
	r.mu.Lock()
	if r.worlds == nil {
		r.worlds = make(map[*world.Cache]*Registry)
	}
	if r.worldUsers == nil {
		r.worldUsers = make(map[*world.Cache]int)
	}
	scope := r.worlds[chunks]
	if scope == nil {
		if len(r.worlds) == 0 {
			scope = r
		} else {
			scope = NewRegistry(r.limit)
			scope.root = r
		}
		r.worlds[chunks] = scope
	}
	r.worldUsers[chunks]++
	r.mu.Unlock()
	return scope, func() {
		r.mu.Lock()
		defer r.mu.Unlock()
		r.worldUsers[chunks]--
		if r.worldUsers[chunks] == 0 {
			delete(r.worldUsers, chunks)
			if scope != r {
				delete(r.worlds, chunks)
			}
		}
	}
}

type writerBarrier struct {
	discard bool
	frame   []byte
	done    chan struct{}
}

type sessionRead struct {
	frame []byte
	err   error
}
