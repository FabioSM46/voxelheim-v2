# Capital castle circulation and room plan

Local coordinates are block cells, X increasing east, Z increasing towards the
southern gate. Standing Y is the player's feet, relative to the building origin.
All coordinate ranges below are inclusive. The envelope remains **63 × 63 × 68**;
city spacing, plateau, ward and spawn clearance therefore retain their dimensions.

## Rooms and protected furnishing pockets

| Wing | Standing Y | Room |
| --- | --- | --- |
| West | 0 / 7 / 14 / 21 | Kitchen/services and carpenter / library / council / upper gallery |
| East | 0 / 7 / 14 / 21 / 28 | Banquet / throne and audience / guard room / bridge gallery / royal overlook |
| Bridge | 21 | West–east crossing over the court, X25–36/Z23–26 |
| Curtain | 13 | Complete circuit and four corner lookouts |

On every west room floor reserve X18–23/Z19–23 and X19–23/Z7–17.
On every east room floor reserve X38–44/Z20–23, X49–55/Z20–23,
X39–42/Z7–10 and X39–42/Z14–18. Solid furniture belongs inside these
pockets. The two banquet groups use the east hall's western/eastern pockets;
the X45–48/Z20–25 central approach remains clear. Cosmetic rugs may cross it.
The carpenter's existing (16,0,22) standing slot remains clear.

## Main stairs and access

West stairs occupy X6–13/Z27–38. Two-wide flights alternate between X7–8
ascending north and X11–12 ascending south, seven metres per flight.
Landings occupy Z27–29 and Z37–38. Guard walls follow both edges of each
flight. The two-wide approach X14–15/Z24–38 connects every landing to its room.
The north route is X17–18/Z6–18, then X16–17/Z19–26.

The east stair reservation is X51–56/Z27–38, with approach X49–50/Z24–38.
The east north route is X37–38/Z6–19, with the future tower-entry corridor
X37–44/Z11–12. Cross-room routes use Z24–25 across X6–24 (west) and
X37–56 (east), joined across the bridge at Y21.

## Tower destinations

| Tower axis X/Z | Entry standing Y | Lookout standing Y | Capital / tip Y |
| --- | --- | --- | --- |
| Northwest 10/12 | 21, west gallery | 35 | 40 / 56 |
| Southwest 20/32 | 21, west gallery | 29 | 34 / 48 |
| Northeast 50/12 | 28, royal overlook | 41 | 46 / 67 |
| Southeast 42/32 | 28, royal overlook | 35 | 40 / 58 |

Elevated entries lead to internal two-wide switchbacks and rooms under the
capitals. Lower supporting masonry remains solid; spire tips are decorative.
Tower furniture pockets will be specified after the completed flights are
verified. Preserve the differentiated silhouettes and the tallest eastern spire.

## Delivery and verification

Issue #1202 is delivered in independently verified changes. The first change
implements **west main-wing stairs and their access only**, together with
oriented schematic placement. East stairs, the widened curtain/corner rooms,
spire interiors and final renderer captures follow in subsequent changes.
The plans above reserve their space; they do not claim that those changes have
already been built.

The west route test drives the real `Player.step` with ordinary walking intent,
production gravity and no jump input, from the gate up all three flights and back
down, in all four rotations. It crosses chunk boundaries and checks overlap,
step height and fall damage. Separate world tests inspect actual placed stair
orientations and all furniture-pocket floor/headroom reservations. World-level
room connectivity continues to guard the rest of the existing castle, including
its still-unconverted routes.
