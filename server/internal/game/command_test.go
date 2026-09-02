package game

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"math"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func commandPlayer(t *testing.T, enabled bool) (*vitalsHarness, *Player, *dropSink) {
	t.Helper()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.sim.devCommands = enabled
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	return h, player, out
}

func TestCommandsDefaultToDisabledAndChangeNothing(t *testing.T) {
	t.Parallel()

	h, player, senderOut := commandPlayer(t, false)
	_, receiverOut := h.join(2, [3]float32{1.5, 64, 0.5})
	beforePosition := player.State().Pos
	beforeInventory := player.InventoryState()

	for _, line := range []string{
		"/additem 1 1",
		"/teleport 0 100 0",
		"/help",
		"/this-server-never-heard-of-it",
		"/help",
		"/help",
	} {
		outcome, err := player.Chat(line)
		if err != nil {
			t.Fatalf("Chat(%q): %v", line, err)
		}
		if !strings.Contains(strings.ToLower(outcome.PrivateText), "disabled") {
			t.Errorf("Chat(%q) answer = %q, want disabled refusal", line, outcome.PrivateText)
		}
		if outcome.Inventory != nil {
			t.Errorf("Chat(%q) returned an inventory change", line)
		}
	}
	if _, err := player.Chat("ordinary chat after the disabled command flood"); !errors.Is(err, ErrChatTooFast) {
		t.Errorf("disabled commands did not spend the shared bucket: %v", err)
	}
	if got := player.State().Pos; got != beforePosition {
		t.Errorf("disabled teleport moved player to %v from %v", got, beforePosition)
	}
	if got := player.InventoryState(); !reflect.DeepEqual(got, beforeInventory) {
		t.Error("disabled additem changed inventory")
	}
	if got := len(chatMessages(t, senderOut)); got != 0 {
		t.Errorf("command reached sender chat delivery %d times", got)
	}
	if got := len(chatMessages(t, receiverOut)); got != 0 {
		t.Errorf("commands reached another player's chat %d times", got)
	}
}

func TestACommandNeverBroadcastsInAnyOutcome(t *testing.T) {
	t.Parallel()

	h, player, senderOut := commandPlayer(t, true)
	_, receiverOut := h.join(2, [3]float32{1.5, 64, 0.5})
	for _, line := range []string{
		"/help",
		"/unknown",
		"/teleport nope 1 2",
		"/additem 65535 1",
	} {
		if _, err := player.Chat(line); err != nil {
			t.Fatalf("Chat(%q): %v", line, err)
		}
	}
	if got := len(chatMessages(t, senderOut)); got != 0 {
		t.Errorf("command path used direct sender chat delivery %d times", got)
	}
	if got := len(chatMessages(t, receiverOut)); got != 0 {
		t.Errorf("command path broadcast %d messages", got)
	}
}

func TestTeleportValidatesEveryArgumentBeforeMoving(t *testing.T) {
	t.Parallel()

	for _, test := range []struct {
		name string
		line string
		word string
	}{
		{name: "zero arguments", line: "/teleport", word: "3 arguments"},
		{name: "two arguments", line: "/teleport 1 2", word: "3 arguments"},
		{name: "four arguments", line: "/teleport 1 2 3 4", word: "3 arguments"},
		{name: "x is not numeric", line: "/teleport east 2 3", word: "x coordinate"},
		{name: "y is not numeric", line: "/teleport 1 high 3", word: "y coordinate"},
		{name: "z is not numeric", line: "/teleport 1 2 north", word: "z coordinate"},
		{name: "positive edge", line: "/teleport 1 2 16777217", word: "world.BlockLimit"},
		{name: "negative edge", line: "/teleport -16777217 2 3", word: "world.BlockLimit"},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, player, _ := commandPlayer(t, true)
			wantPosition := player.State().Pos
			outcome, err := player.Chat(test.line)
			if err != nil {
				t.Fatalf("Chat: %v", err)
			}
			if !strings.Contains(outcome.PrivateText, test.word) {
				t.Errorf("answer = %q, want it to name %q", outcome.PrivateText, test.word)
			}
			if got := player.State().Pos; got != wantPosition {
				t.Errorf("refused teleport moved player to %v", got)
			}
		})
	}
}

func TestTeleportMovesAuthoritativelyAndWakesChunkStreaming(t *testing.T) {
	t.Parallel()

	_, player, _ := commandPlayer(t, true)
	if _, err := player.NextChunk(context.Background()); err != nil {
		t.Fatalf("initial NextChunk: %v", err)
	}
	player.haveTick, player.lastTick = true, 90
	player.haveMineTick, player.lastMineTick = true, 91
	player.haveAttackTick, player.lastAttackTick = true, 92
	player.haveLootOpenTick, player.lastLootOpenTick = true, 93
	player.haveLootTakeTick, player.lastLootTakeTick = true, 94
	player.haveLootTakeAllTick, player.lastLootTakeAllTick = true, 95
	player.haveTradeTick, player.lastTradeTick = true, 96

	outcome, err := player.Chat("/teleport 96 80 -65")
	if err != nil {
		t.Fatalf("Chat: %v", err)
	}
	if !strings.Contains(outcome.PrivateText, "96 80 -65") {
		t.Errorf("answer = %q, want destination", outcome.PrivateText)
	}
	state := player.State()
	if state.Pos != [3]float32{96, 80, -65} || state.Vel != [3]float32{} || state.OnGround {
		t.Errorf("teleported state = %+v", state)
	}
	wantChunk := world.ChunkOf(96, 80, -65)
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if got, err := player.NextChunk(ctx); err != nil || got != wantChunk {
		t.Fatalf("NextChunk after teleport = %v, %v; want %v", got, err, wantChunk)
	}
	if player.haveTick || player.haveMineTick || player.haveAttackTick || player.haveLootOpenTick ||
		player.haveLootTakeTick || player.haveLootTakeAllTick || player.haveTradeTick {
		t.Error("teleport retained a per-message ordering guard")
	}
}

func TestAddItemUsesOrdinaryStackingAndIsAllOrNothing(t *testing.T) {
	t.Parallel()

	_, player, _ := commandPlayer(t, true)
	player.inventory.mu.Lock()
	player.inventory.slots[1] = stackOf(ItemStone, 63)
	player.inventory.mu.Unlock()

	outcome, err := player.Chat("/additem 1 2")
	if err != nil {
		t.Fatalf("Chat: %v", err)
	}
	if outcome.Inventory == nil {
		t.Fatal("accepted additem returned no inventory state")
	}
	if got := outcome.Inventory.Stacks[1]; got.ItemID != uint16(ItemStone) || got.Count != 64 {
		t.Errorf("partial stack after additem = %+v", got)
	}
	if got := outcome.Inventory.Stacks[2]; got.ItemID != uint16(ItemStone) || got.Count != 1 {
		t.Errorf("next stack after additem = %+v", got)
	}

	player.inventory.mu.Lock()
	for slot := range player.inventory.slots[:equipmentFirst] {
		player.inventory.slots[slot] = stackOf(ItemDirt, 64)
	}
	player.inventory.mu.Unlock()
	before := player.InventoryState()
	refusal, err := player.Chat("/additem 1 1")
	if err != nil {
		t.Fatalf("full-pack Chat: %v", err)
	}
	if !strings.Contains(refusal.PrivateText, "does not fit") || refusal.Inventory != nil {
		t.Errorf("full-pack outcome = %+v", refusal)
	}
	if got := player.InventoryState(); !reflect.DeepEqual(got, before) {
		t.Error("full-pack additem changed inventory")
	}
}

func TestAddItemValidatesIDCountAndCapacity(t *testing.T) {
	t.Parallel()

	for _, test := range []struct {
		line string
		word string
	}{
		{line: "/additem", word: "2 arguments"},
		{line: "/additem 1", word: "2 arguments"},
		{line: "/additem 1 1 extra", word: "2 arguments"},
		{line: "/additem stone 1", word: "item id"},
		{line: "/additem 65535 1", word: "unknown"},
		{line: "/additem 1 none", word: "count"},
		{line: "/additem 1 0", word: "greater than zero"},
		{line: "/additem 35 none", word: "count"},
		{line: "/additem 35 0", word: "greater than zero"},
		{line: "/additem 1 2305", word: "capacity"},
	} {
		_, player, _ := commandPlayer(t, true)
		before := player.InventoryState()
		outcome, err := player.Chat(test.line)
		if err != nil {
			t.Fatalf("Chat(%q): %v", test.line, err)
		}
		if !strings.Contains(strings.ToLower(outcome.PrivateText), strings.ToLower(test.word)) {
			t.Errorf("Chat(%q) answer = %q, want %q", test.line, outcome.PrivateText, test.word)
		}
		if got := player.InventoryState(); !reflect.DeepEqual(got, before) {
			t.Errorf("Chat(%q) changed inventory", test.line)
		}
	}
}

func TestAddItemSilverUpdatesThePurseWithoutUsingASlot(t *testing.T) {
	t.Parallel()

	_, player, _ := commandPlayer(t, true)
	before := player.InventoryState()
	outcome, err := player.Chat("/additem 35 1000")
	if err != nil {
		t.Fatalf("Chat: %v", err)
	}
	if outcome.PrivateText != "Added 1000 silver." {
		t.Errorf("answer = %q, want silver confirmation", outcome.PrivateText)
	}
	if outcome.Inventory == nil {
		t.Fatal("accepted silver additem returned no inventory state")
	}
	if outcome.Inventory.Silver != before.Silver+1000 {
		t.Errorf("returned silver = %d, want %d", outcome.Inventory.Silver, before.Silver+1000)
	}
	if !reflect.DeepEqual(outcome.Inventory.Stacks, before.Stacks) {
		t.Error("silver additem changed an inventory slot")
	}
	if got := player.InventoryState(); !reflect.DeepEqual(got, *outcome.Inventory) {
		t.Errorf("authoritative inventory = %+v, want returned state %+v", got, *outcome.Inventory)
	}
}

func TestAddItemSilverChecksOverflowBeforeMutation(t *testing.T) {
	t.Parallel()

	_, player, _ := commandPlayer(t, true)
	player.inventory.mu.Lock()
	player.inventory.silver = math.MaxUint32 - 1000
	player.inventory.mu.Unlock()

	accepted, err := player.Chat("/additem 35 1000")
	if err != nil {
		t.Fatalf("exact-boundary Chat: %v", err)
	}
	if accepted.Inventory == nil || accepted.Inventory.Silver != math.MaxUint32 {
		t.Fatalf("exact-boundary outcome = %+v, want a full purse", accepted)
	}

	before := player.InventoryState()
	refusal, err := player.Chat("/additem 35 1")
	if err != nil {
		t.Fatalf("overflow Chat: %v", err)
	}
	if !strings.Contains(refusal.PrivateText, "overflow") || refusal.Inventory != nil {
		t.Errorf("overflow outcome = %+v", refusal)
	}
	if got := player.InventoryState(); !reflect.DeepEqual(got, before) {
		t.Errorf("overflow changed inventory to %+v from %+v", got, before)
	}
}

func TestHelpUnknownAndTheSharedRateLimitArePrivate(t *testing.T) {
	t.Parallel()

	h, player, _ := commandPlayer(t, true)
	_, receiverOut := h.join(2, [3]float32{1.5, 64, 0.5})
	help, err := player.Chat("/help")
	if err != nil {
		t.Fatalf("help: %v", err)
	}
	for _, shape := range []string{"/help", "/teleport <x> <y> <z>", "/additem <item-id> <count>"} {
		if !strings.Contains(help.PrivateText, shape) {
			t.Errorf("help = %q, missing %q", help.PrivateText, shape)
		}
	}
	unknown, err := player.Chat("/dance")
	if err != nil || !strings.Contains(unknown.PrivateText, "Unknown command") {
		t.Fatalf("unknown outcome = %+v, %v", unknown, err)
	}
	for spent := 2; spent < ChatBurst; spent++ {
		if _, err := player.Chat("/help"); err != nil {
			t.Fatalf("command %d: %v", spent+1, err)
		}
	}
	if _, err := player.Chat("/additem 1 1"); !errors.Is(err, ErrChatTooFast) {
		t.Fatalf("command after burst error = %v, want ErrChatTooFast", err)
	}
	if got := len(chatMessages(t, receiverOut)); got != 0 {
		t.Errorf("private answers broadcast %d lines", got)
	}
}

func TestAcceptedCommandsAreInfoLoggedWithActorAndArguments(t *testing.T) {
	t.Parallel()

	_, player, _ := commandPlayer(t, true)
	var output bytes.Buffer
	player.sim.log = slog.New(slog.NewTextHandler(&output, &slog.HandlerOptions{Level: slog.LevelInfo}))
	if _, err := player.Chat("/teleport 1 70 2"); err != nil {
		t.Fatalf("Chat: %v", err)
	}
	if _, err := player.Chat("/additem 35 1000"); err != nil {
		t.Fatalf("silver Chat: %v", err)
	}
	line := output.String()
	for _, want := range []string{"level=INFO", "entity_id=1", "command=/teleport", "arguments=\"[1 70 2]\""} {
		if !strings.Contains(line, want) {
			t.Errorf("log %q does not contain %q", line, want)
		}
	}
	for _, want := range []string{"command=/additem", "arguments=\"[35 1000]\""} {
		if !strings.Contains(line, want) {
			t.Errorf("log %q does not contain %q", line, want)
		}
	}
}
