package game

import (
	"errors"
	"fmt"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// What the ore in a pack is worth, and the only copy of it.
//
// **The recipe table is deliberately not sent to clients**, for the reason `itemRegistry`
// is not: a client may mirror a display-only copy so it can gray out a row nobody can
// afford, and a drift between the two copies can show a wrong label but can never create
// an item. The wire carries a `RecipeID` and nothing else — no ingredient list, no
// product, no station — so there is no claim here for the server to disbelieve.
//
// There is no acknowledgement either. A craft that happened is answered by the complete
// `InventoryState` that follows it, and one that did not is answered by silence.

// ForgeCraftRadius is how close a player must be to a forge to use it, in blocks.
//
// Measured with `distanceToVoxel` — body centre to the centre of the forge's anchor voxel
// — which is the reach convention every other rule on this side already uses. A second
// way of measuring distance is a second answer to "am I near enough", and the two would
// disagree at exactly the boundary players stand on.
//
// Five rather than `EditReach`'s 4.5: standing *at* a forge is a looser idea than reaching
// a voxel with your hands, and a radius under the reach would make a forge you can touch
// one you cannot work at.
const ForgeCraftRadius = 5.0

// ingredient is one line of a recipe's cost.
type ingredient struct {
	item  ItemID
	count uint16
}

// recipe is what one `RecipeID` costs, what it yields and where it can be made.
//
// station is `StructureKindUnknown` for a recipe that needs none, which is the same
// fail-closed zero the wire uses — a recipe that forgets to name its station needs no
// station, and that is the only direction in which forgetting is harmless here: the
// station-less recipes are the ones a player must be able to make with nothing built yet.
type recipe struct {
	ingredients  []ingredient
	product      ItemID
	productCount uint16
	station      vnet.StructureKind
}

// recipeTable is the authoritative answer to every `CraftRequest`.
//
// Keyed by the wire's `RecipeID` rather than by a name, so an unknown key and the
// absent-field `Unknown` are one lookup and one refusal. `RecipeIDUnknown` is deliberately
// absent from the table for exactly that reason.
//
// The six recipes are a chain rather than a list: logs make a tent and a fire, stone and
// coal make the forge, the forge is what turns raw iron into a blade and stone into the
// means of keeping it, and what a hunt leaves behind is the other way of keeping it —
// mended where you are standing rather than where you built. Nothing here smelts — raw
// iron and coal go straight to the edge, with the coal as the fuel — because an
// intermediate metallurgy chain is a later design and this one has to be playable first.
var recipeTable = map[vnet.RecipeID]recipe{
	// No station: the forge is the thing you build before you have one.
	vnet.RecipeIDForge: {
		ingredients:  []ingredient{{ItemStone, 8}, {ItemRawCoal, 2}},
		product:      ItemForge,
		productCount: 1,
	},

	// No station either, and for the same reason read from the other end: a tent is where
	// a player comes back to, and needing a forge to make one would mean dying at the join
	// spawn until the ore ran out.
	vnet.RecipeIDTent: {
		ingredients:  []ingredient{{ItemLog, 8}},
		product:      ItemTent,
		productCount: 1,
	},

	// No station for the third time, and the reason is the tent's read once more: a fire
	// is one of the things a player builds before they have anything, and the ground it
	// keeps clear is worth most on the first night — which is the night nobody has a forge.
	//
	// Four logs and one raw coal: under the tent's eight logs, because a fire is the
	// cheaper half of making a spot survivable and a camp is allowed several of them.
	vnet.RecipeIDCampfire: {
		ingredients:  []ingredient{{ItemLog, 4}, {ItemRawCoal, 1}},
		product:      ItemCampfire,
		productCount: 1,
	},

	// The blade the whole chain is for. It arrives at full durability because
	// `slotTable.insert` builds it through `stackOf`, which reads the registry — not
	// because anything here says so.
	vnet.RecipeIDIronSword: {
		ingredients:  []ingredient{{ItemRawIron, 3}, {ItemRawCoal, 2}, {ItemLog, 1}},
		product:      ItemIronSword,
		productCount: 1,
		station:      vnet.StructureKindForge,
	},

	// What keeps the blade alive. The repair itself is a field action and needs no forge;
	// this is only where the stones are made.
	vnet.RecipeIDSharpeningStone: {
		ingredients:  []ingredient{{ItemStone, 2}, {ItemRawCoal, 1}},
		product:      ItemSharpeningStone,
		productCount: 1,
		station:      vnet.StructureKindForge,
	},

	// The other half of that job, and the fourth recipe that needs nowhere to stand —
	// which is the whole difference between it and the stone above. Both mend a blade;
	// one is made at a forge out of what you dug, the other is made where you are
	// standing out of what you killed. A station here would mean walking home to make the
	// kit whose point is not having to.
	//
	// Two pelts and nothing else. A vargr leaves one, so a patch costs two hunts — the
	// price is the hunting rather than anything in this row.
	vnet.RecipeIDLeatherPatch: {
		ingredients:  []ingredient{{ItemVargrPelt, 2}},
		product:      ItemLeatherPatch,
		productCount: 1,
	},
}

// craft spends a recipe's ingredients and inserts its product, or changes nothing.
//
// **All or nothing, and it gets that by working on a copy.** Every ingredient is consumed
// from a scratch table and the product is inserted into the same scratch table; the real
// slots are replaced only once every step has succeeded. That is what makes "materials
// *and* room for the output verified before anything is consumed" true without a
// would-this-fit predicate sitting beside the insertion rule and disagreeing with it — the
// check *is* the insertion, run somewhere it can be thrown away.
//
// The order matters and is the reason the copy is worth its cost: the ingredients come out
// first, so a pack whose every slot is full still crafts when the recipe empties one. A
// room check performed before the spend would refuse that, and refusing it is the bug the
// copy exists to avoid rather than a rule anybody chose.
func (t *slotTable) craft(r recipe) bool {
	scratch := *t

	for _, needed := range r.ingredients {
		if !scratch.consume(needed.item, needed.count) {
			return false
		}
	}
	if remaining := scratch.insert(r.product, r.productCount); remaining != 0 {
		return false
	}

	*t = scratch
	return true
}

// stationWithinLocked reports whether a structure of this kind stands within radius of a
// position, whoever owns it.
//
// **Ownership is deliberately not consulted.** A forge is a place, not a possession: any
// player may work at any forge they can walk to, and the owner field exists for removal
// and respawn. A camp somebody else built being useful is the cooperative half of a
// cooperative game.
//
// A scan of the registry rather than of voxels, and O(structures) per craft — which is a
// handful of entries at a frequency measured in seconds, on the same explicit trade the
// drops, the mobs and the tent lookup already record.
//
// The caller holds Sim.mu.
func (s *Sim) stationWithinLocked(kind vnet.StructureKind, pos [3]float64, radius float64) bool {
	for _, held := range s.structures {
		if held.kind != kind {
			continue
		}
		if distanceToVoxel(pos, held.anchorVoxel()) <= radius {
			return true
		}
	}
	return false
}

// Craft resolves one CraftRequest against the authoritative pack and the authoritative
// world, and returns the inventory a successful craft produced.
//
// Every refusal is an ordinary error the session logs at debug and answers with silence:
// there is no rejection message in the contract, and a client learns its craft did not
// happen by its pack not changing.
//
// **One critical section, for the reason placement has one.** Liveness, the station scan
// and the slot arithmetic are one decision, and splitting them would leave a window in
// which the player walks away from the forge between the check and the spend. Nothing in
// it blocks: the registry scan is arithmetic, and the inventory is taken with TryLock —
// the same discipline the tick uses, which is what keeps the pair deadlock-free by
// construction rather than by lock ordering.
func (p *Player) Craft(req protocol.CraftRequest) (protocol.InventoryState, error) {
	// The lookup is the whole of the validation of `recipe`. `RecipeIDUnknown` is the
	// absent-field case and is not in the table; a value no member has is a client
	// speaking a contract this server does not. Both are one missing key and one silence.
	r, known := recipeTable[req.Recipe]
	if !known {
		return protocol.InventoryState{}, fmt.Errorf("recipe %s is not one this server knows", req.Recipe)
	}

	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if !p.alive() {
		// Consistent with mining, editing, placing and attacking: a corpse does nothing.
		return protocol.InventoryState{}, errors.New("the player is dead")
	}

	if r.station != vnet.StructureKindUnknown {
		if !p.sim.stationWithinLocked(r.station, p.pos, ForgeCraftRadius) {
			return protocol.InventoryState{}, fmt.Errorf("no %s stands within %.1f blocks", r.station, ForgeCraftRadius)
		}
	}

	// TryLock, never Lock, and the same argument placement records: every other holder of
	// this inventory is either this session's own read goroutine or the tick, and the tick
	// only ever takes it under the lock this function is holding. Written as a refusal
	// anyway, because "cannot fail" is a property of today's callers rather than of the
	// lock.
	if !p.inventory.mu.TryLock() {
		return protocol.InventoryState{}, errors.New("the inventory is busy")
	}
	defer p.inventory.mu.Unlock()

	if !p.inventory.slots.craft(r) {
		return protocol.InventoryState{}, fmt.Errorf("the pack has no room or not enough for %s", req.Recipe)
	}

	p.sim.log.Debug("craft applied",
		"entity_id", p.entityID, "recipe", req.Recipe.String(),
		"product", uint16(r.product), "client_tick", req.ClientTick)

	return p.inventory.stateLocked(), nil
}
