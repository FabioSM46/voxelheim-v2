package game

import (
	"fmt"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

const commandHelp = "/help | /teleport <x> <y> <z> | /additem <item-id> <count>"

// commandLocked parses one slash-prefixed line under Sim.mu.
func (p *Player) commandLocked(line string) ChatOutcome {
	if !p.sim.devCommands {
		return privateCommand("Development commands are disabled.")
	}
	if len(line) > MaxChatBytes {
		return privateCommand(fmt.Sprintf("Command is longer than the %d-byte limit.", MaxChatBytes))
	}
	if !utf8.ValidString(line) {
		return privateCommand("Command has to be valid text.")
	}
	for _, character := range line {
		if unicode.IsControl(character) {
			return privateCommand("Command may not contain control characters.")
		}
	}

	fields := strings.Fields(line)
	if len(fields) == 0 {
		return privateCommand("Unknown command.")
	}
	name, args := fields[0], fields[1:]

	var outcome ChatOutcome
	var accepted bool
	switch name {
	case "/help":
		if len(args) != 0 {
			outcome = privateCommand(fmt.Sprintf("/help takes 0 arguments; got %d.", len(args)))
			break
		}
		outcome, accepted = privateCommand(commandHelp), true
	case "/teleport":
		outcome, accepted = p.teleportCommandLocked(args)
	case "/additem":
		outcome, accepted = p.addItemCommandLocked(args)
	default:
		return privateCommand("Unknown command.")
	}

	if accepted {
		outcome.command = name
		outcome.arguments = args
	}
	return outcome
}

func privateCommand(text string) ChatOutcome { return ChatOutcome{PrivateText: text} }

func (p *Player) teleportCommandLocked(args []string) (ChatOutcome, bool) {
	if len(args) != 3 {
		return privateCommand(fmt.Sprintf("/teleport needs 3 arguments <x> <y> <z>; got %d.", len(args))), false
	}
	if err := p.cannotActLocked(); err != nil {
		return privateCommand(fmt.Sprintf("/teleport refused: %s.", err)), false
	}

	var destination [3]float64
	for axis, argument := range args {
		coordinate, err := strconv.ParseInt(argument, 10, 64)
		if err != nil {
			return privateCommand(fmt.Sprintf("/teleport %s coordinate %q is not a whole number.", axisName(axis), argument)), false
		}
		if coordinate < -world.BlockLimit || coordinate > world.BlockLimit {
			return privateCommand(fmt.Sprintf("/teleport %s coordinate %d is outside world.BlockLimit (%d).", axisName(axis), coordinate, world.BlockLimit)), false
		}
		destination[axis] = float64(coordinate)
	}

	// Use the exact target even inside a solid; do not relocate it.
	p.pos = destination
	p.vel = [3]float64{}
	p.onGround = false
	p.current = intent{yaw: p.current.yaw}
	p.idleTicks = 0
	p.setMiningLocked(nil)
	p.pendingSwing = nil
	p.blocking = false

	// Reset ordering guards across the discontinuity and wake chunk streaming.
	p.haveTick, p.lastTick = false, 0
	p.haveMineTick, p.lastMineTick = false, 0
	p.haveAttackTick, p.lastAttackTick = false, 0
	p.haveLootOpenTick, p.lastLootOpenTick = false, 0
	p.haveLootTakeTick, p.lastLootTakeTick = false, 0
	p.haveLootTakeAllTick, p.lastLootTakeAllTick = false, 0
	p.haveTradeTick, p.lastTradeTick = false, 0
	p.chunk = chunkAt(p.pos)
	p.chunks.publish(p.chunk)

	return privateCommand(fmt.Sprintf("Teleported to %d %d %d.", int64(destination[0]), int64(destination[1]), int64(destination[2]))), true
}

func axisName(axis int) string {
	return [...]string{"x", "y", "z"}[axis]
}

func (p *Player) addItemCommandLocked(args []string) (ChatOutcome, bool) {
	if len(args) != 2 {
		return privateCommand(fmt.Sprintf("/additem needs 2 arguments <item-id> <count>; got %d.", len(args))), false
	}
	if err := p.cannotActLocked(); err != nil {
		return privateCommand(fmt.Sprintf("/additem refused: %s.", err)), false
	}

	rawID, err := strconv.ParseUint(args[0], 10, 16)
	if err != nil {
		return privateCommand(fmt.Sprintf("/additem item id %q is not a number in 1..65535.", args[0])), false
	}
	itemID := ItemID(rawID)
	definition, registered := itemByID(itemID)
	if !registered || itemID == ItemNone {
		return privateCommand(fmt.Sprintf("/additem item id %d is unknown.", rawID)), false
	}

	rawCount, err := strconv.ParseUint(args[1], 10, 16)
	if err != nil {
		return privateCommand(fmt.Sprintf("/additem count %q is not a number in 1..65535.", args[1])), false
	}
	if rawCount == 0 {
		return privateCommand("/additem count must be greater than zero."), false
	}
	maxCount := uint64(equipmentFirst) * uint64(definition.maxStack)
	if rawCount > maxCount {
		return privateCommand(fmt.Sprintf("/additem count %d exceeds this pack's capacity of %d for item id %d.", rawCount, maxCount, rawID)), false
	}

	if !p.inventory.mu.TryLock() {
		return privateCommand("/additem refused: the inventory is busy."), false
	}
	defer p.inventory.mu.Unlock()

	// Reuse the ordinary all-or-nothing insertion. Durable objects occupy one stack
	// each, so insert them individually and restore the slot table on failure.
	before := p.inventory.slots
	inserted := false
	if definition.maxDurability == 0 {
		inserted = p.inventory.insertWholeStackLocked(inventoryStack{item: itemID, count: uint16(rawCount)})
	} else {
		inserted = true
		for range rawCount {
			if !p.inventory.insertWholeStackLocked(stackOf(itemID, 1)) {
				inserted = false
				break
			}
		}
	}
	if !inserted {
		p.inventory.slots = before
		return privateCommand("/additem refused: the whole request does not fit in the pack."), false
	}

	state := p.inventory.stateLocked()
	return ChatOutcome{
		PrivateText: fmt.Sprintf("Added %d of item id %d.", rawCount, rawID),
		Inventory:   &state,
	}, true
}
