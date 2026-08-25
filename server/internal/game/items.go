package game

import "github.com/FabioSM46/voxelheim-v2/server/internal/world"

// ItemID names one kind of thing an inventory stack may hold. It is deliberately
// a different type from world.Block: some items place no voxel at all, and some
// blocks yield an item other than themselves.
type ItemID uint16

const (
	// ItemNone is the wire representation of an empty inventory slot. It is never
	// registered and can never be inserted as an item.
	ItemNone ItemID = iota
	ItemStone
	ItemDirt
	ItemSnow
	ItemLog
	ItemRawCoal
	ItemRawIron

	// ItemRustySword is the starter weapon, and the first item in the game that wears
	// out. Appended, never inserted: every id above is already on the wire in a
	// client's inventory, and iota renumbers everything after an insertion.
	ItemRustySword

	// ItemForge and ItemTent are the first items that put an *entity* in the world
	// rather than a voxel. Appended for the reason the sword was, and pinned by a test:
	// the client mirrors these two numbers to draw a held shape, and iota renumbers
	// everything after an insertion.
	ItemForge
	ItemTent

	// ItemIronSword is the first weapon a player makes rather than is given, and
	// ItemSharpeningStone is what keeps one alive. Appended for the reason every id above
	// was, and pinned by a test: iota renumbers everything after an insertion, and these
	// two are on the wire in a client's inventory the moment somebody crafts one.
	ItemIronSword
	ItemSharpeningStone

	// ItemCampfire is the third item that plants an entity rather than a voxel, and the
	// first whose point is the ground *around* it rather than the cell it stands on.
	// Appended for the reason every id above was, and pinned by a test: iota renumbers
	// everything after an insertion, and the client mirrors these numbers to draw a held
	// shape.
	ItemCampfire

	// The first three items in this game that come off a creature rather than out of the
	// ground: what a draugr and a vargr leave behind, and what two pelts are worked into.
	// Appended for the reason every id above was, and pinned by a test: iota renumbers
	// everything after an insertion, and the client mirrors these numbers to draw a pack.
	ItemBone
	ItemVargrPelt
	ItemLeatherPatch

	// The first three items that are neither a weapon nor a resource: implements, each
	// for one family of ground. Appended for the reason every id above was, and pinned by
	// the same test: iota renumbers everything after an insertion, and the client mirrors
	// these numbers to draw a held shape and an icon.
	ItemShovel
	ItemPickaxe
	ItemAxe

	// The first food ingredient, and the first item the hunger system can consume.
	ItemRawMeat

	// The cooked form is appended rather than inserted because item ids cross the wire.
	// It is a distinct food and the product of the campfire recipe.
	ItemCookedMeat

	// Two complete armour sets, appended rather than inserted because item ids cross the
	// wire. Each piece names one of the three equipment slots introduced in V18.
	ItemLeatherCap
	ItemLeatherJerkin
	ItemLeatherLeggings
	ItemIronHelm
	ItemIronCuirass
	ItemIronGreaves
)

// What each blade is worth, and the only copy of it.
//
// **Item stats rather than world constants, which is why they live beside the registry**
// — the rule `RustySwordMaxDurability` established and the reason it was written down:
// the next durable item chooses its own, and nothing about any of these numbers
// generalises. `SwordReach`, `SwordConeDegrees` and `SwordCooldown` stayed in
// constants.go, because those describe the *swing* and every blade swings the same way.
//
// The damages are what a landed hit costs a draugr, whose 60 health is the scale they are
// read against: three rusty swings kill one, two iron ones do. That is the whole of what
// the upgrade buys in combat, and it is deliberately a step rather than a multiplier.
const (
	RustySwordMaxDurability uint16 = 100
	RustySwordDamage        uint16 = 25

	// The iron blade lasts twice as long and hits harder. Both halves matter: a weapon
	// that only hit harder would make the rusty one strictly obsolete on the first swing,
	// where this one is worth the ore because it is worth *carrying*.
	IronSwordMaxDurability uint16 = 200
	IronSwordDamage        uint16 = 40

	// What the three implements are worth, on the same terms the blades are: their own
	// numbers, beside the registry, generalising to nothing.
	//
	// **Two hundred each, and they are all the same on purpose.** A blade's durability is
	// a step in a progression — rusty to iron — and there is no such ladder here: one
	// shovel, one pickaxe, one axe, which is what #185 decided when it ruled out tiers. A
	// difference between them would be a difference nobody chose, and the first thing
	// somebody would try to read a ladder into.
	//
	// The iron blade's 200 is the scale, and it is the right one to borrow: a tool costs
	// ore like the blade does, and **nothing in this game wears from use** — death is the
	// only wear there is — so what this number buys is how many deaths an implement
	// survives rather than how many blocks it breaks. See #199, which is where that
	// penalty is being narrowed to what a player has on them.
	ToolMaxDurability uint16 = 200

	// Leather survives on the rusty blade's scale; iron survives twice as long. A set is
	// three separate objects, so every piece carries the same maximum and its own wear.
	LeatherArmourMaxDurability uint16 = 100
	IronArmourMaxDurability    uint16 = 200

	// SharpeningStoneRestore is how much wear one stone gives back, and it sits here for
	// the reason the numbers above do: it describes the *stone*, not the act of repairing.
	//
	// Fifty against the rusty blade's 100 and the iron one's 200 makes a stone worth half
	// a starter sword or a quarter of a forged one — which is what keeps GDD §4's "limited
	// supplies" a matter of the stack cap and the crafting cost rather than a special rule.
	//
	// A flat amount rather than a fraction of the maximum, deliberately. A fraction would
	// cost the same number of stones to keep either blade alive, so the reward for forging
	// the better one would silently include a cheaper upkeep, and the amount would stop
	// being readable from the item that carries it.
	SharpeningStoneRestore uint16 = 50

	// LeatherPatchRestore is what one patch gives back, and it sits here for the reason
	// the stone's amount does: it describes the *patch*.
	//
	// Forty against the stone's fifty, and the gap is deliberately small — it is not what
	// separates them. GDD §4 gives the game two field kits and the difference between
	// them is where they come from: a stone is made at a forge out of stone and coal,
	// which is a walk back to camp; a patch is made anywhere out of two pelts, which is
	// something that had to be hunted. A multiplier would make one of them the answer and
	// the other a mistake. Forty and fifty make them two answers to the same question,
	// chosen by what the player has on them rather than by which is better.
	LeatherPatchRestore uint16 = 40

	// RawMeatHungerRestore is the reserve one piece of uncooked deer meat gives
	// back. It describes the item, so it lives beside the other item-owned values
	// and is read through itemRegistry rather than by an item-id check in Consume.
	RawMeatHungerRestore uint16 = 25

	// CookedMeatHungerRestore is the full reserve one cooked piece gives back. Cooking
	// changes the item rather than multiplying Consume's result, so the value remains a
	// registry fact like raw meat's.
	CookedMeatHungerRestore uint16 = 100
)

// itemDefinition is the server-only rule for one item. places is world.Air when
// the item cannot be placed; maxStack belongs to the item rather than to the
// inventory so a later item can choose a different carrying limit.
type itemDefinition struct {
	places   world.Block
	maxStack uint16

	// wornAt names the one equipment slot this item may enter: head, chest or legs.
	// Zero means it cannot be worn, which is every existing registry row. The move
	// rule reads this column in both directions before changing either slot.
	wornAt wornAt

	// armour is the percentage points of incoming damage this worn piece will remove.
	// This issue only records the value: three leather pieces sum to 15%, turning a
	// draugr's 10 into 8 and a vargr's 7 into 5; three iron pieces sum to 30%, turning
	// those same hits into 7 and 4. Damage application belongs to the combat issue.
	armour uint16

	// threat is tenths added to the mob aggro weight while this piece is worn. Leather
	// adds none; three iron pieces add fifteen tenths to the base weight of one, so a
	// fully iron-clad player weighs 2.5 times as much in a mob's choice. Aggro applies it
	// later; the registry merely owns the item stat here.
	threat uint16

	// maxDurability is zero for an item that does not wear out, which is every
	// resource and therefore every stack in the game until this one. A non-zero value
	// is what makes an item *equipment*: one whole item to a slot, never merged and
	// never split, carried with a current wear beside it that only the server changes.
	//
	// The zero is load-bearing in the same way the wire's is. schemas/player.fbs reads
	// a `(0, 0)` durability pair as "this slot holds nothing that wears out", so an
	// item that forgets to declare a maximum is a resource rather than an
	// indestructible weapon.
	maxDurability uint16

	// meleeDamage is what one landed swing with this item costs, and zero for everything
	// that is not a weapon — which is every resource, every structure and the empty slot.
	//
	// The zero is what makes "is this a weapon" a registry question rather than a list of
	// item ids in the combat code. A swing that named a stack of stone used to be refused
	// by comparing against ItemRustySword; it is now refused by the stone having no damage
	// to do, which is the same answer and one that a third weapon does not have to be
	// added to.
	meleeDamage uint16

	// repairRestore is how much durability one of this item gives back when it is spent
	// mending something, and zero for everything that is not a repair kit — which today
	// is every item but the sharpening stone and the leather patch.
	//
	// A registry field for the reason meleeDamage is one, and it is the same lesson: a
	// swing that named a stack of stone used to be refused by comparing against an item
	// id, and is now refused by the stone having no damage to do. "Is this a repair kit"
	// is a registry question in exactly that shape, which is why adding the leather patch
	// required one entry here and no edit to the repair path.
	//
	// The zero is fail-closed and load-bearing: an item that says nothing about repair
	// cannot be spent as a kit, and there is deliberately no second list of kit ids
	// anywhere that could disagree with this field.
	repairRestore uint16

	// restoresHunger is how much reserve one of this item gives back when eaten,
	// and zero for everything that is not food. The zero is fail-closed: a new item
	// cannot be consumed until its registry row deliberately says it is edible.
	restoresHunger uint16
}

// wornAt is the server-only placement class for worn equipment. Its zero value refuses
// equipment slots, so adding an item without deliberately naming a body location cannot
// make it wearable.
type wornAt uint8

const (
	wornNowhere wornAt = iota
	wornHead
	wornChest
	wornLegs
)

// itemRegistry is intentionally not sent to clients. They receive authoritative
// slot contents and may render an opinion about them, but only this table decides
// whether an item places a block and how many fit in a stack.
var itemRegistry = map[ItemID]itemDefinition{
	ItemStone:   {places: world.Stone, maxStack: 64},
	ItemDirt:    {places: world.Dirt, maxStack: 64},
	ItemSnow:    {places: world.Snow, maxStack: 64},
	ItemLog:     {places: world.Log, maxStack: 64},
	ItemRawCoal: {places: world.Air, maxStack: 64},
	ItemRawIron: {places: world.Air, maxStack: 64},

	// Places no block and stacks with nothing, including another sword: two blades are
	// two objects with two different amounts of wear left, and a stack of two could
	// only carry one of those numbers.
	ItemRustySword: {places: world.Air, maxStack: 1, maxDurability: RustySwordMaxDurability, meleeDamage: RustySwordDamage},

	// Structures place no *block*. `places: world.Air` is what says so, exactly as it
	// does for a raw resource — what they put in the world is an entity, and the path
	// that does it is PlaceStructure rather than the ordinary edit. One to a slot,
	// because a camp is carried one shelter at a time and a stack of tents would be a
	// stack of things that each want their own anchor. Nothing about them wears out:
	// zero maxDurability is the wire's "(0, 0) — this slot holds nothing that wears".
	ItemForge: {places: world.Air, maxStack: 1},
	ItemTent:  {places: world.Air, maxStack: 1},

	// The fire is a structure on exactly those terms — no block, one to a slot, nothing
	// that wears out. What it does once it is standing is keep the ground around it
	// clear of spawns, and that is a rule about the *placed* structure rather than about
	// the item, so nothing here says it.
	ItemCampfire: {places: world.Air, maxStack: 1},

	// The forge's own products. The blade is equipment on the same terms the rusty one
	// is: one to a slot, its own wear, its own damage.
	ItemIronSword: {places: world.Air, maxStack: 1, maxDurability: IronSwordMaxDurability, meleeDamage: IronSwordDamage},

	// A stone is a consumable, not equipment: it wears nothing out because spending it
	// *is* how it is used, and eight to a stack is what makes "limited supplies" a
	// carrying decision rather than a special rule. Its own maxDurability stays zero for
	// that reason — what it restores is somebody else's wear, which is repairRestore.
	ItemSharpeningStone: {places: world.Air, maxStack: 8, repairRestore: SharpeningStoneRestore},

	// What the dead leave behind. Plain resources on the terms every raw material here is
	// held: they place no block, they wear out nothing, and they do nothing to a mob —
	// three zeroes that are each the documented meaning of the field rather than a row
	// somebody left unfinished.
	//
	// Sixty-four bones to a stack against sixteen pelts, and that difference is the only
	// thing separating two otherwise identical rows: a bone is a small thing to carry a
	// lot of, and a hide is not.
	//
	// **Nothing consumes a bone yet, and that is deliberate rather than unfinished.** It
	// is the reagent GDD §7's engraving table will want, and registering it now is what
	// lets a draugr leave one behind before the bench that spends it exists. A resource
	// with no sink is a resource; the alternative was a creature that leaves nothing.
	ItemBone:      {places: world.Air, maxStack: 64},
	ItemVargrPelt: {places: world.Air, maxStack: 16},

	// The field kit a hunt pays for. A consumable exactly as the sharpening stone is —
	// its own maxDurability stays zero because spending it *is* how it is used, and what
	// it restores is somebody else's wear — and eight to a stack for the same reason.
	//
	// **This one field is the whole of the integration with the repair path.** repair.go
	// compares against no item id: it asks what the named slot restores, and a non-zero
	// answer is what makes something a kit. See repairRestore's own comment, which
	// described this row before it existed.
	ItemLeatherPatch: {places: world.Air, maxStack: 8, repairRestore: LeatherPatchRestore},

	// The three implements. Equipment on exactly the blades' terms: they place no block,
	// one to a slot because two shovels are two objects with two different amounts of wear
	// left, and their own durability.
	//
	// **No `meleeDamage`, and that zero is the row's only statement about combat.** A
	// pickaxe is not a bad sword — it is not a sword — and the zero is what says so
	// through the same registry question `meleeDamage` was made into, rather than through
	// a list of ids somewhere in the combat path.
	//
	// Which ground each one is for is *not* here: that is `toolFamilies` in mining.go,
	// beside the costs it multiplies, because a tool's speed is a fact about breaking a
	// block rather than about the item.
	ItemShovel:  {places: world.Air, maxStack: 1, maxDurability: ToolMaxDurability},
	ItemPickaxe: {places: world.Air, maxStack: 1, maxDurability: ToolMaxDurability},
	ItemAxe:     {places: world.Air, maxStack: 1, maxDurability: ToolMaxDurability},

	// The first food. It places nothing, wears out nothing and does no damage; sixteen
	// to a stack keeps it carryable without making a carcass indistinguishable from a
	// block resource. Its one non-zero capability is the registry answer Consume reads.
	ItemRawMeat: {places: world.Air, maxStack: 16, restoresHunger: RawMeatHungerRestore},

	// Cooking keeps the hunted resource's stack shape and changes only what eating it is
	// worth. It is wearless, harmless and not placeable by the registry's zero values.
	ItemCookedMeat: {places: world.Air, maxStack: 16, restoresHunger: CookedMeatHungerRestore},

	// Field-worked leather: one whole durable piece for each body slot. Five points each
	// make the complete set fifteen percent armour, and no threat keeps it neutral.
	ItemLeatherCap:      {places: world.Air, maxStack: 1, wornAt: wornHead, armour: 5, maxDurability: LeatherArmourMaxDurability},
	ItemLeatherJerkin:   {places: world.Air, maxStack: 1, wornAt: wornChest, armour: 5, maxDurability: LeatherArmourMaxDurability},
	ItemLeatherLeggings: {places: world.Air, maxStack: 1, wornAt: wornLegs, armour: 5, maxDurability: LeatherArmourMaxDurability},

	// Forged iron: twice the durability and armour of leather. Five threat tenths per
	// piece make the complete set add 1.5 to the base aggro weight, hence 2.5 times.
	ItemIronHelm:    {places: world.Air, maxStack: 1, wornAt: wornHead, armour: 10, threat: 5, maxDurability: IronArmourMaxDurability},
	ItemIronCuirass: {places: world.Air, maxStack: 1, wornAt: wornChest, armour: 10, threat: 5, maxDurability: IronArmourMaxDurability},
	ItemIronGreaves: {places: world.Air, maxStack: 1, wornAt: wornLegs, armour: 10, threat: 5, maxDurability: IronArmourMaxDurability},
}

// blockDrops is the authoritative answer to a successful break. ItemNone is an
// explicit yield for Leaves; an absent entry also means no yield, so new blocks
// fail closed until their drop is deliberately chosen.
var blockDrops = map[world.Block]ItemID{
	world.Stone:   ItemStone,
	world.Dirt:    ItemDirt,
	world.Grass:   ItemDirt,
	world.Snow:    ItemSnow,
	world.Log:     ItemLog,
	world.Leaves:  ItemNone,
	world.CoalOre: ItemRawCoal,
	world.IronOre: ItemRawIron,
}

// blockExperience is the lifetime progress a successful break earns. It mirrors every
// breakable row in blockDrops, including the explicit zeroes: adding a block without
// deciding its reward is a registry error rather than an implicit choice of none.
var blockExperience = map[world.Block]uint16{
	world.Stone:   0,
	world.Dirt:    0,
	world.Grass:   0,
	world.Snow:    0,
	world.Log:     2,
	world.Leaves:  0,
	world.CoalOre: 4,
	world.IronOre: 6,
}

func itemByID(id ItemID) (itemDefinition, bool) {
	item, ok := itemRegistry[id]
	return item, ok
}

// blockPlacedBy returns the block this item may place. The registry chooses the
// mapping and the block palette keeps the final say over whether that block is a
// player-placeable voxel.
func blockPlacedBy(id ItemID) (world.Block, bool) {
	item, ok := itemByID(id)
	if !ok || item.places == world.Air || !world.Placeable(item.places) {
		return world.Air, false
	}
	return item.places, true
}

func itemDroppedBy(block world.Block) ItemID { return blockDrops[block] }
