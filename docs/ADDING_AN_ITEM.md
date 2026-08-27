# Adding an Item

The path one new item takes through this repository, in the order it has to be walked, with
the rule that decides each step and a pointer to where that rule is written down.

**This file is a path, not a second copy of the rules.** Every rule here is enforced somewhere
— by a type, by a test, or by a fail-closed zero in a table — and the enforcing copy is named
beside it. Where this document and the code disagree, the code is right and this file is a
bug. That is deliberate: two copies of a rule drift, and a checklist that quietly stops being
true is worse than no checklist, because somebody follows it.

Read `server/AGENTS.md` and `client/AGENTS.md` before starting. They own the reasoning; this
owns the order.

## Two rules that decide most of the work

**Append, never insert.** Item ids cross the wire and sit inside inventories that are already
persisted. The Go side is an `iota` block, so an insertion silently renumbers every id below
it — including the ones a saved pack still refers to — and the client mirrors several of those
numbers by hand. A new item goes at the *end* of the const block, and its number is never
reused, not even for an item that was deleted.

**Adding an item is not a schema change.** An item id crosses the wire as a `uint16` inside
messages that already exist, and neither the item registry nor the recipe table is ever sent —
clients receive authoritative slot contents and render an opinion about them. So there is no
`.fbs` edit, no `flatc` regeneration and no protocol bump. If your item needs a *new kind of
message* — a new request, a new event — that is a schema change and a different piece of work,
and `schemas/AGENTS.md` owns it.

## Server first — the item exists there or it does not exist

### 1. The id

Append a constant to the `iota` block in `server/internal/game/items.go`, with a comment
saying what the item is and why it is appended. Every existing id carries that comment; they
are not decoration, they are the record of a rule that costs a save file if it is broken.

### 2. The registry row

Add one row to `itemRegistry` in the same file. Every column is optional in the sense that its
zero value is a complete answer, and **every zero fails closed**:

| Column | Zero means |
| --- | --- |
| `places` | `world.Air` — places no voxel. Structures are `Air` too: what they put in the world is an entity |
| `maxStack` | Must be set. One to a slot for anything carrying its own wear |
| `wornAt` | Cannot be worn — an item is not equipment until a body location is named |
| `maxDurability` | Does not wear out. The wire reads `(0, 0)` as "nothing that wears" |
| `meleeDamage` | Not a weapon. This is what makes "is it a weapon" a registry question |
| `repairRestore` | Not a repair kit |
| `restoresHunger` | Not food |
| `launches` / `ammunition` | Not a launcher |

**A capability is a column, never a comparison against an id.** This is the single most
repeated lesson in this codebase, and it has been paid for twice: a swing used to be refused by
comparing the slot against `ItemRustySword`, and a repair used to be refused by a list of kit
ids. Both are now registry fields, which is why the iron sword and the leather patch each cost
one row and no edit to the path that acts on them. If your item needs a new *kind* of
capability, add a column with a fail-closed zero — not a branch.

### 3. The item's own numbers

Damage, durability, restore amounts and the like are named constants **beside the registry**,
not in `constants.go`. The distinction is written out in `items.go`: `SwordReach` and
`SwordCooldown` describe the *swing*, which every blade shares, while `IronSwordDamage`
describes the *item* and generalises to nothing. Each constant carries the reasoning for its
value, on the scale it is read against.

### 4. How it enters the world

An item nobody can obtain is not in the game. There are four channels, and a new item uses at
least one:

- **The starter pack** — `starterSlots` in `server/internal/game/inventory.go`
- **A recipe** — the table in `server/internal/game/craft.go`, which is server-only and
  deliberately never sent
- **A broken block** — `blockDrops` in `items.go`, whose test requires every breakable block to
  have a row, including the explicit zeroes
- **A dead creature** — `loot: []lootRoll{...}` on a species in
  `server/internal/game/species.go`

## Client second — presentation only, and against the server's actual behaviour

### 5. Where the id is declared

**The module that *acts* on an id declares it; ids nothing acts on are declared in
`client/src/player/items.rs`.** The blade lives in `client/src/player/combat.rs`, the three bundles in
`client/src/player/structures.rs`, the forge's products in `client/src/player/crafting.rs`, and every plain block, material and
mob drop lives in `items.rs` because drawing them is all anyone does. One declaration read from
several places cannot drift the way two declarations of the same number can.

### 6. The display row

Add one row to the table in `items.rs`: a name, an `ItemShape` and an `ItemColour`. Every
reader on the client goes through it — the held view model, the cell, the recipe panel, the
tooltip — and there is nowhere else to add half of one.

**Nothing in that row is ever a gameplay fact.** Drawing an item as a `Blade` does not make the
left button swing it, and a client-side copy of what an item can do is a cheat vector however
carefully it is written.

### 7. Reuse a shape before inventing one

`ItemShape` is a vocabulary of *kinds*, not a picture per item: three implements share one
`Tool` silhouette and are told apart by colour, and every wearable piece shares one `Armour`
plate. Reach for an existing variant first.

A genuinely new shape is a larger change than it looks: both renderers match on `ItemShape`
with no wildcard arm, so a new variant does not compile until it has a held mesh **and** a cell
drawing, and `client/src/player/drops.rs` builds one world mesh per variant as well. That is the design
working, not an obstacle — but budget for three drawings, not one.

### 8. Route it, if anything routes on it

A weapon is an entry in `combat::LEFT_BUTTON_USES`; a structure is an entry in
`client/src/player/structures.rs`; a repair kit is an entry in `client/src/player/inventory.rs`'s kit table. Each of these lists
**fails open toward asking**: the server re-reads its own registry for every request, so an id
wrongly listed costs a refused request and nothing else, while an id wrongly *omitted* costs
the feature. That asymmetry is why the lists exist at all, and it is recorded where they are.

## The four surfaces one item is drawn on

| Surface | Drawn by |
| --- | --- |
| In the hand, first person | `client/src/player/hands.rs` |
| On the ground | `client/src/player/drops.rs` |
| In the body's fist, third person | `client/src/player/drops.rs`, through `client/src/player/mod.rs` |
| Inventory and hotbar cell | `client/src/ui/icon.rs` |

Three renderers, four places. They agree because all of them read `items.rs` for what the item
*is* and key their drawing on `ItemShape` rather than on an item id — so a new item that reuses
a shape is drawn correctly in all four without touching any of them.

**An item-level exception is drawn once and forgotten in three places.** The rusty sword's rust
is the standing example: it is reached by an `item_id ==` comparison inside `client/src/player/hands.rs`, so for
a long time it existed in first person and nowhere else, and nothing measured the gap. If your
item genuinely needs a per-item visual, ask what the other three surfaces will show.

## What fails if you skip a step

You do not have to remember this list. These tests fail when a step is missed, and knowing
which one is failing tells you which step it was:

- `TestEveryItemIsRegisteredWithItsOwnStackLimitAndPlacement` (server) — an id with no
  registry row
- `every_known_item_has_a_name_a_shape_and_a_colour` (client) — a display row missing a fact,
  or filled in with a placeholder
- the contiguity check in the same client sweep — a duplicate id or a hole, which is what an
  insertion or a reused number looks like from here
- `the_registry_names_every_item_id_this_client_declares` (client) — an id declared in a module
  and never added to the display table, the one direction the table cannot see about itself
- `every_shape_has_a_drawing_of_its_own` (client) — a new shape answered with an empty drawing
  or a copy of another one

**They catch omissions, not wrong values.** A registry row with the wrong damage, a colour that
is not the one you meant, a recipe that costs too little: nothing here will tell you. That is
what review and playing the game are for.

## What a new item must never do

- Take an id by insertion, or reuse the id of a deleted item
- Carry a gameplay rule on the client — what an item does is the server's registry
- Introduce a second list of ids that answers a question a registry column already answers
- Declare the same id in two places on the client
- Require a schema change without one: if it needs a new message, do that work properly

## Checklist

```
Server
[ ] id appended to the iota block, with the comment saying why it is appended
[ ] itemRegistry row, every capability a column and every zero deliberate
[ ] the item's own numbers, named, beside the registry, with their reasoning
[ ] at least one way to obtain it: starter pack, recipe, block drop or loot roll
[ ] go test ./... green

Client
[ ] id declared in the module that acts on it, or in items.rs if nothing does
[ ] one display row: name, shape, colour
[ ] an existing ItemShape reused, or three drawings written for a new one
[ ] routed, if anything routes on it
[ ] cargo test --workspace --locked green

Both
[ ] no .fbs edit and no gen/ regeneration, unless a new message was genuinely needed
[ ] the Definition of Done gates in AGENTS.md, for every workspace the change touched
```
