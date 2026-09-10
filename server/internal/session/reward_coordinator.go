package session

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
)

// The boss reward coordinator is the asynchronous owner of one claim per character, from
// the Store barrier to the durable acknowledgement (#1036). It runs every disk step on its
// own goroutine with no simulation or inventory lock held, and calls a live game method
// only between those steps. No gameplay producer submits a claim yet.
//
// A claim owns its character's last word. While it is pending the autosave skips the
// character, offline experience stays queued, and a teardown hands the leaving life to the
// claim instead of writing it. The account claim is kept until the claim's final write, so
// a reconnect is refused rather than resumed from a life the reward has not reached.

var (
	// ErrRewardsDisabled refuses a claim in a world with no durable reward journal.
	ErrRewardsDisabled = errors.New("session: boss rewards are not enabled in this world")
	// ErrRewardOwned refuses a second claim while the character already has one.
	ErrRewardOwned = errors.New("session: the character already has a pending boss reward")
	// ErrRewardsDraining refuses a claim once shutdown has begun draining.
	ErrRewardsDraining = errors.New("session: boss rewards are draining for shutdown")
	// ErrRewardNotPlaying refuses a claim for a character without a live session.
	ErrRewardNotPlaying = errors.New("session: a boss reward names no playing character")
)

const (
	rewardRetryMin = 50 * time.Millisecond
	rewardRetryMax = 2 * time.Second
)

// BossRewardClaim is one already-rolled entitlement for a connected character.
type BossRewardClaim struct {
	Self       Resolved
	Player     *game.Player
	Generation uint64
	Boss       vnet.MobKind
	Grant      game.BossRewardGrant
}

type rewardCoordinator struct {
	journal *persist.RewardStore
	manager *game.InstanceManager

	// ctx is cancelled only when a shutdown drain runs out of time. A claim then stops
	// retrying and keeps its ownership: its durable intent is startup recovery's.
	ctx    context.Context
	cancel context.CancelFunc
	wg     sync.WaitGroup

	// closing is guarded by Identities.mu.
	closing bool

	retryMin, retryMax time.Duration
	// step is a test seam called before each stage, with no lock held. Nil in production.
	step func(stage string)
}

// rewardTask is one claim's ownership. Its mutex orders the claim's live steps against a
// teardown, and is never held across disk I/O except the single reservation read. Lock
// order, on every path including ClaimBossReward: rewardTask.mu before Identities.mu, the
// manager, Sim and inventory.
type rewardTask struct {
	mu       sync.Mutex
	self     Resolved
	player   *game.Player
	claim    *game.BossRewardClaim
	token    *persist.RewardReservation
	ref      persist.RewardReference
	detached *rewardDetachment
	closed   bool
}

// rewardDetachment is the leaving character's last word, held until the claim ends.
type rewardDetachment struct {
	live   game.Life // as the simulation last held it, instance coordinates included
	disk   game.Life // what the final record stores: the portal return, for a visit
	portal bool
	// write is false for an external binding with no portal visit. It has no known return
	// point, so the reward's durable postimage stands as the record instead.
	write bool
}

// EnableRewards starts the coordinator over the world's reward journal. An ephemeral world
// has no durable barrier and is refused.
func (i *Identities) EnableRewards(journal *persist.RewardStore, manager *game.InstanceManager) error {
	if journal == nil || manager == nil || i.store.Dir() == "" {
		return ErrRewardsDisabled
	}
	i.mu.Lock()
	defer i.mu.Unlock()
	if i.rewards != nil {
		return errors.New("session: boss rewards are already enabled")
	}
	ctx, cancel := context.WithCancel(context.Background())
	i.rewards = &rewardCoordinator{
		journal: journal, manager: manager, ctx: ctx, cancel: cancel,
		retryMin: rewardRetryMin, retryMax: rewardRetryMax,
	}
	return nil
}

// ClaimBossReward takes ownership of the character and delivers the claim asynchronously.
// The channel receives exactly one result. A refusal before the intent is sealed leaves
// the character exactly as it was; a failure after it keeps ownership, which only a
// successful retry or startup recovery ends.
func (i *Identities) ClaimBossReward(req BossRewardClaim) (<-chan error, error) {
	if req.Player == nil || req.Self.Character.IsZero() {
		return nil, ErrRewardNotPlaying
	}
	task := &rewardTask{self: req.Self, player: req.Player}
	// Taken before Identities.mu, in the documented order. It stays held until the
	// reservation resolves, so a teardown waits to learn whether the claim owns it.
	task.mu.Lock()
	refuse := func(err error) (<-chan error, error) {
		i.mu.Unlock()
		task.mu.Unlock()
		return nil, err
	}

	i.mu.Lock()
	c := i.rewards
	switch held, live := i.live[req.Self.ID]; {
	case c == nil:
		return refuse(ErrRewardsDisabled)
	case c.closing:
		return refuse(ErrRewardsDraining)
	case !live || held.finalised || held.character != req.Self.Character:
		return refuse(ErrRewardNotPlaying)
	case held.reward != nil:
		return refuse(ErrRewardOwned)
	default:
		held.reward = task
	}
	c.wg.Add(1)
	i.mu.Unlock()

	done := make(chan error, 1)
	go func() {
		defer c.wg.Done()
		done <- i.runReward(c, task, req)
	}()
	return done, nil
}

// DrainRewards refuses new claims and waits for pending ones until ctx ends. A claim still
// pending then stops retrying and keeps its Store barrier, account and durable intent.
func (i *Identities) DrainRewards(ctx context.Context) error {
	i.mu.Lock()
	c := i.rewards
	if c != nil {
		c.closing = true
	}
	i.mu.Unlock()
	if c == nil {
		return nil
	}
	done := make(chan struct{})
	go func() {
		c.wg.Wait()
		close(done)
	}()
	select {
	case <-done:
		return nil
	case <-ctx.Done():
		c.cancel()
		return fmt.Errorf("session: boss rewards were still pending; their intents remain for startup recovery: %w", ctx.Err())
	}
}

func (i *Identities) runReward(c *rewardCoordinator, t *rewardTask, req BossRewardClaim) error {
	if err := i.reserveReward(c, t, req); err != nil {
		return err
	}
	stages := []struct {
		name string
		run  func() error
	}{
		{"sealed", func() error {
			return i.durably(c, func() error { return c.journal.PrepareClaim(i.store, t.token, t.ref) })
		}},
		{"prepared", func() error { return i.durably(c, func() error { return i.store.WritePreparedReward(t.token) }) }},
		{"written", func() error { return i.publishReward(c, t) }},
		{"published", func() error {
			return i.durably(c, func() error { return c.journal.AcknowledgeClaim(i.store, t.token, t.ref) })
		}},
		{"acknowledged", func() error { return i.finishReward(c, t) }},
	}
	for _, stage := range stages {
		if c.step != nil {
			c.step(stage.name)
		}
		if err := stage.run(); err != nil {
			i.log.Error("a boss reward stalled; the character stays owned until it resolves",
				"player_id", t.self.ID.Short(), "stage", stage.name, "error", err)
			return err
		}
	}
	return nil
}

// reserveReward installs the Store barrier, captures the live postimage and seals both
// halves. It runs with t.mu held and releases it. Every refusal before sealing undoes
// exactly what it took.
func (i *Identities) reserveReward(c *rewardCoordinator, t *rewardTask, req BossRewardClaim) error {
	character := req.Self.Character
	token, durable, err := i.store.ReserveReward(character)
	if err != nil {
		i.endRewardLocked(t)
		return err
	}
	claim, image, err := c.manager.ReserveBossReward(t.player, lifeOfRecord(durable), req.Grant)
	if err != nil {
		err = errors.Join(err, i.store.AbortReward(token))
		i.endRewardLocked(t)
		return err
	}
	ref := persist.RewardReference{
		Generation: req.Generation, Boss: req.Boss,
		Owner:   persist.SessionCharacter{PlayerID: req.Self.ID, CharacterID: uint64(character)},
		Entries: image.Entries, Silver: image.Silver, Experience: image.Experience,
	}
	// The postimage keeps the baseline's identity and LastSeen. The journal compares
	// intents whole, and a fresh timestamp would not survive its encoding unchanged.
	postimage := durable
	applyLife(&postimage, image.Life)
	intent := persist.RewardIntent{RewardReference: ref, PreviousEpoch: durable.BossRewardEpoch, Postimage: postimage}
	err = c.journal.ValidateClaim(intent)
	if err == nil {
		err = i.store.BeginRewardIntent(token, postimage)
	}
	if err != nil {
		err = errors.Join(err, t.player.AbortBossReward(claim), i.store.AbortReward(token))
		i.endRewardLocked(t)
		return err
	}
	t.claim, t.token, t.ref = claim, token, ref
	if _, err := claim.Seal(); err != nil {
		// Unreachable: this task is the claim's only holder. The Store half is sealed,
		// so ownership is kept rather than guessed about.
		t.mu.Unlock()
		return err
	}
	t.mu.Unlock()
	return nil
}

// durably retries one idempotent disk transition until it lands or the drain gives up.
// A refusal that names the reservation itself will not change on retry.
func (i *Identities) durably(c *rewardCoordinator, op func() error) error {
	wait := c.retryMin
	for {
		if err := c.ctx.Err(); err != nil {
			return err
		}
		err := op()
		if err == nil {
			return nil
		}
		if errors.Is(err, persist.ErrRewardReservation) || errors.Is(err, persist.ErrRewardPending) || errors.Is(err, persist.ErrRewardEpoch) {
			return err
		}
		i.log.Warn("a boss reward write failed; retrying", "error", err, "retry_in", wait.String())
		select {
		case <-c.ctx.Done():
			return errors.Join(err, c.ctx.Err())
		case <-time.After(wait):
		}
		wait = min(wait*2, c.retryMax)
	}
}

// publishReward follows the durable character write. A connected owner receives the image
// live; a detached one has it applied to the life it left behind, and a remembered portal
// visit is replaced with that published life before anything can resume it.
func (i *Identities) publishReward(c *rewardCoordinator, t *rewardTask) error {
	t.mu.Lock()
	defer t.mu.Unlock()
	if err := c.ctx.Err(); err != nil {
		return err
	}
	if t.detached == nil {
		return t.player.PublishBossReward(t.claim)
	}
	d := t.detached
	published, err := t.claim.PublishRemembered(t.owner(), d.live)
	if err != nil {
		return err
	}
	if d.portal {
		c.manager.ReplaceDisconnectedLife(t.owner(), d.live, published)
	}
	disk := published
	disk.Pos = d.disk.Pos
	d.live, d.disk = published, disk
	return nil
}

// finishReward follows the durable acknowledgement. The Store barrier is released only
// after the claim has finished, and the character is released only after its last word.
func (i *Identities) finishReward(c *rewardCoordinator, t *rewardTask) error {
	t.mu.Lock()
	if err := c.ctx.Err(); err != nil {
		t.mu.Unlock()
		return err
	}
	var err error
	if t.detached == nil {
		err = t.player.FinishBossReward(t.claim)
	} else {
		err = t.claim.FinishRemembered(t.owner(), t.detached.live)
	}
	if err == nil {
		err = i.store.ReleaseReward(t.token)
	}
	if err != nil {
		t.mu.Unlock()
		return err
	}
	i.endRewardLocked(t)
	return nil
}

// endRewardLocked ends a task's ownership. It is called with t.mu held and releases it.
// A detached character's final record is written before its account is released; an
// external binding keeps the reward's durable postimage instead.
func (i *Identities) endRewardLocked(t *rewardTask) {
	t.closed = true
	d := t.detached
	if d == nil {
		i.clearReward(t)
		t.mu.Unlock()
		return
	}
	t.mu.Unlock()

	i.writeMu.Lock()
	var err error
	if d.write {
		err = i.write(t.self.Character, d.disk, t.self.Explored, t.self.Marks)
	}
	if d.write && err == nil {
		i.mu.Lock()
		delete(i.portalReturns, t.self.Character)
		i.mu.Unlock()
	}
	i.clearReward(t)
	i.writeMu.Unlock()
	if err != nil {
		i.log.Error("the player's record was not saved", "player_id", t.self.ID.Short(), "error", err)
	}
	i.Release(t.self.ID)
}

func (i *Identities) clearReward(t *rewardTask) {
	i.mu.Lock()
	defer i.mu.Unlock()
	if held := i.live[t.self.ID]; held != nil && held.reward == t {
		held.reward = nil
	}
}

// detachReward hands a leaving character's last word to its pending boss reward. It
// reports false when there is none, and the teardown then writes and releases as always.
// Called after sim.Leave; on true the claim owns the record write and the release.
// writeRecord is false when the character leaves an external binding with no known
// return point, whose record must not receive its coordinates.
func (i *Identities) detachReward(self Resolved, player *game.Player, portal *game.PortalEntry, instances *game.InstanceManager, writeRecord bool) bool {
	i.mu.Lock()
	var t *rewardTask
	if held := i.live[self.ID]; held != nil && held.character == self.Character {
		t = held.reward
	}
	i.mu.Unlock()
	if t == nil || t.player != player {
		return false
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.closed {
		return false
	}
	life := player.Record()
	disk := life
	if portal != nil {
		i.rememberPortalReturn(self, portal.Return)
		instances.DisconnectPortal(*portal, life)
		for axis, value := range portal.Return {
			disk.Pos[axis] = float64(value)
		}
	}
	t.detached = &rewardDetachment{live: life, disk: disk, portal: portal != nil, write: writeRecord}
	i.finalise(self.ID)
	return true
}

func (t *rewardTask) owner() game.InstanceCharacter {
	return game.InstanceCharacter{PlayerID: t.self.ID, CharacterID: uint64(t.self.Character)}
}
