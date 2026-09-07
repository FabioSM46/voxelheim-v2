"""Rebuild the study's box meshes and orthographic sheet using only Python's stdlib.

Run from any directory: python3 docs/first-dungeon/prototype.py.
The OBJ files are disposable collision/rig proxies, never production assets.
Coordinates are blocks, Y up and forward -Z, with the origin at the feet.
"""

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent

# name, centre, dimensions. Each box is one articulated proxy segment. Final
# authoring batches static detail onto these rather than adding per-detail draws.
GUARDIAN = [
    ("pelvis", (0, .85, .38), (.8, .5, .5)),
    ("thorax", (0, 1.18, -.08), (1.2, .8, .7)),
    ("neck", (0, 1.08, -.37), (.7, .65, .35)),
    ("head", (0, .95, -.59), (.6, .5, .4)),
    ("jaw", (0, .69, -.61), (.5, .12, .36)),
    ("tail", (0, .67, .69), (.18, .35, .2)),
    ("ruff_left", (-.35, 1.61, -.08), (.35, .38, .5)),
    ("ruff_right", (.35, 1.61, -.08), (.35, .38, .5)),
]
for x in [-.58, .58]:
    for z in [-.34, .39]:
        GUARDIAN += [
            (f"upper_{x}_{z}", (x, .68, z), (.3, .6, .3)),
            (f"lower_{x}_{z}", (x, .22, z), (.34, .44, .36)),
        ]
for x in [-.4, 0, .4]:
    GUARDIAN.append((f"chain_{x}", (x, .68, -.45), (.07, .32, .07)))

KING = [
    ("pelvis", (0, 1.08, 0), (.55, .36, .42)),
    ("thorax", (0, 1.85, 0), (.62, 1.05, .5)),
    ("head_crown", (0, 2.54, 0), (.4, .52, .4)),
    ("blade", (.41, 1.1, -.32), (.12, 1.65, .08)),
    ("armour_left", (-.25, 2.17, 0), (.28, .22, .55)),
    ("armour_right", (.25, 2.17, 0), (.28, .22, .55)),
]
for x in [-.39, .39]:
    KING += [
        (f"upper_arm_{x}", (x, 1.93, 0), (.2, .5, .25)),
        (f"forearm_{x}", (x, 1.5, -.05), (.2, .4, .25)),
    ]
for x in [-.17, .17]:
    KING += [
        (f"thigh_{x}", (x, .79, 0), (.25, .53, .3)),
        (f"shin_{x}", (x, .27, -.04), (.28, .54, .4)),
    ]
for x in [-.27, 0, .27]:
    KING.append((f"cloak_{x}", (x, 1.22, .37), (.22, 1.65, .16)))


def corners(centre, size):
    return [tuple(c + s * sign / 2 for c, s, sign in zip(centre, size, signs))
            for signs in [(-1,-1,-1), (1,-1,-1), (1,1,-1), (-1,1,-1),
                          (-1,-1,1), (1,-1,1), (1,1,1), (-1,1,1)]]


FACES = [(0,3,2,1), (4,5,6,7), (0,4,7,3), (1,2,6,5), (0,1,5,4), (3,7,6,2)]


def build(name, parts, width, height, row):
    vertices = [v for _, centre, size in parts for v in corners(centre, size)]
    low = [min(v[a] for v in vertices) for a in range(3)]
    high = [max(v[a] for v in vertices) for a in range(3)]
    assert low[0] >= -width / 2 and high[0] <= width / 2
    assert low[2] >= -width / 2 and high[2] <= width / 2
    assert low[1] >= 0 and high[1] <= height + 1e-9
    obj = ["# First dungeon study proxy; units are blocks, Y up, facing -Z"]
    for n, (part, centre, size) in enumerate(parts):
        obj.append(f"g {part}")
        obj.extend("v " + " ".join(f"{v:.5f}" for v in point) for point in corners(centre, size))
        for a,b,c,d in FACES:
            for tri in [(a,b,c), (a,c,d)]:
                obj.append("f " + " ".join(str(n*8 + i + 1) for i in tri))
    (ROOT / f"{name}-proxy.obj").write_text("\n".join(obj) + "\n")
    svg = []
    for view, axis in enumerate([0, 2, 0]):
        cx, base = 190 + view * 320, 370 + row * 355
        svg.append(f'<text x="{cx}" y="{base+27}" text-anchor="middle">{name}: {["front", "side", "rear"][view]}</text>')
        # Painter order is depth, so overlapping boxes remain readable.
        depth = 2 if axis == 0 else 0
        ordered = sorted(parts, key=lambda p: p[1][depth], reverse=view != 2)
        for i, (part, centre, size) in enumerate(ordered):
            color = ["#394850", "#607079", "#81919a"][i % 3]
            x = cx + 95 * (centre[axis] - size[axis]/2)
            y = base - 95 * (centre[1] + size[1]/2)
            svg.append(f'<rect x="{x}" y="{y}" width="{95*size[axis]}" height="{95*size[1]}" fill="{color}" stroke="#202c33" stroke-width="1"/>')
        svg.append(f'<rect x="{cx-95*width/2}" y="{base-95*height}" width="{95*width}" height="{95*height}" fill="none" stroke="#b15e3c" stroke-dasharray="5 4"/>')
        # Same 95 px/block projection: companion player box is never an art estimate.
        svg.append(f'<rect x="{cx+100}" y="{base-95*1.8}" width="{95*.6}" height="{95*1.8}" fill="none" stroke="#356876"/><text x="{cx+100}" y="{base+16}" font-size="12">player</text>')
    return svg, {"segments": len(parts), "triangles": len(parts)*12,
                 "vertices": len(vertices), "material_slots": 1, "effects": 0,
                 "min": low, "max": high}


def main():
    svg = ['<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1150 780"><rect width="1150" height="780" fill="#efeee8"/><g font-family="sans-serif" font-size="18" fill="#25343b"><text x="35" y="35" font-size="25">Rig proxies — three views at one scale</text><text x="35" y="65">Simple segment envelopes, not final meshes. Orange: body. Blue: player.</text>']
    report = {}
    for row, (name, parts, width, height) in enumerate([
        ("guardian", GUARDIAN, 1.6, 1.8), ("king", KING, 1, 2.8)
    ]):
        views, counts = build(name, parts, width, height, row)
        svg.extend(views)
        report[name] = counts
    svg.append('</g></svg>')
    (ROOT / 'proxies.svg').write_text(''.join(svg))
    (ROOT / 'proxy-counts.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
