# Spatial prototype inspection

Measured 2026-09-07 on AMD Ryzen 7 3700X, x86_64 Linux. The study uses
`Player.step` and the production swept collision code, with separate test-only
29 x 29 and 33 x 33 arena fixtures. The shipped chamber is unchanged.

Reproduce movement: from `server/`, run
`go test ./internal/game -run TestDungeonStudy -v`.
Rebuild geometry: from the repository root, run
`python3 docs/first-dungeon/prototype.py`.

## Measured geometry

The [three-view sheet](proxies.svg) renders the actual box descriptors exported as
[guardian OBJ](guardian-proxy.obj) and [king OBJ](king-proxy.obj). Each box has twelve
outward-facing triangles. These are simple study meshes, with no final ornament,
damage or animation implementation. [Machine-readable counts](proxy-counts.json)
come from those descriptors, not from the budget caps.

| Proxy | Segments | Triangles | Vertices | Material slots | Effects |
| --- | --- | --- | --- | --- | --- |
| Guardian | 19 | 228 | 152 | 1 | 0 |
| King | 17 | 204 | 136 | 1 | 0 |

Guardian extents: X -0.75..0.75, Y 0..1.8, Z -0.79..0.79.
King extents: X -0.49..0.49, Y 0..2.8, Z -0.36..0.45.
The script asserts every vertex remains within the corresponding rest envelope.
Both fit the five-block-wide, five-block-high gallery; a two-block-high lintel
rejects the king. The collision test sweeps each admitted body through nine blocks
of gallery, rather than checking its dimensions alone.

## Measured escape clearance

Values are distance from the starting centre to the trailing edge of the whole
player footprint after ordinary lateral movement. Each preparation reserves 0.25 s
plus one tick; remaining time rounds down to complete simulation ticks. Inputs
are refreshed every tick. No jump, mount, sprint, damage or invulnerability.

| Tick rate | Hunger | 0.9 s preparation | 1.2 s preparation | 1.5 s preparation |
| --- | --- | --- | --- | --- |
| 20 Hz | zero | 1.764 | 2.796 | 3.828 |
| 20 Hz | fed | 2.280 | 3.570 | 4.860 |
| 60 Hz | zero | 1.879 | 2.853 | 3.943 |
| 60 Hz | fed | 2.423 | 3.642 | 5.003 |

Both arena fixtures produce these central clear-floor results. A 1.5-block lateral
target passes every sampled case. A 2-block target **fails** at 0.9 s with zero
hunger at both rates, and passes at 1.2 s. Thus the study admits 1.2 s as the initial
minimum for a 2-block landing/sector escape under the stated allowance. It does
not establish the fairness of any unimplemented move or combination.

A direct path into the monolith at (8,8) stops short and fails the escape target;
the perpendicular path towards the central cross clears two blocks. Outer walls
stop the player rather than allowing the nominal speed calculation to carry it
outside. The low lintel and too-short preparation are negative controls, not
success cases omitted from the report.

## Recorded visual inspection

The generated turnarounds, corrected pose sheets and Chrome-rendered orthographic
proxy sheet were inspected. The proxy views use exactly the same scale for both
bosses and the adjacent player envelope. All feet and the king's crown fit the
rest envelope; the separate blade and cloak remain inside it. The guardian's broad,
low head/shoulder profile and the king's narrow vertical profile remain distinct.

The concept sheets are art references, not measurable meshes. Their miniatures
do not supply scale. Numbered repeated drawings illustrate sequential poses rather
than extra creatures or weapons. The written pose index defines the full sequence.
The king storyboard's first left-cut stroke is schematic rather than a complete
figure, so the left/right/thrust preparations must remain the authoring reference.

No final rig motion, extreme-pose clipping, gameplay-camera capture, audio audition
or GPU frame time was measured here. Those belong to the implementation reviews
of #1019 and #1031 and the later full animation work. The initial segment, triangle,
material and effect caps in the design remain authoring budgets, not FPS promises.
