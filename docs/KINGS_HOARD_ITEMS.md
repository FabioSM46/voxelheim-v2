# King's hoard item handoff

The server owns these rows and their progression. The client mirrors their presentation and
recipe in `player/items.rs` and `player/crafting.rs`; attack intent routing includes the runic
sword in `player/combat.rs`. These client rows grant no gameplay capability.

| ID | Server item | Display name | Existing shape for #1316 | Presentation |
| --- | --- | --- | --- | --- |
| 47 | `ItemKingRune` | King's Rune | `ItemShape::Material` | A cold violet reagent, distinct from grey iron and dark coal |
| 48 | `ItemRunicSword` | Runic Sword | `ItemShape::Blade` | A pale violet blade, distinct from the iron sword |

Both reuse existing shapes in the hand, world drop, remote hand and inventory cell. No new
shape, asset or renderer exception is needed. The rune belongs in the client item display
registry; the sword belongs in the crafting declarations and the existing left-button weapon
routing. Display rows do not determine gameplay capabilities.

The rune stacks to 64 and has no placement, equipment, wear, food, repair or mount capability.
Every eligible member receives one rune from the Draugr king's personal loot, in addition to the
existing iron sword and bones. The runic sword occupies one main-hand slot, leaves the shield
hand free, deals 50 base melee damage and has 300 maximum durability, against iron's 40 and 200.

`RecipeID.RunicSword` consumes one iron sword and one King's Rune at an Enchanting Table and
awards 10 crafting experience, matching other station recipes. The existing crafting rule
accepts ingredients from carried and equipped slots, regardless of current wear. The result
arrives in the pack at full durability. The reagent is the unlock; no learned-recipe state exists.
