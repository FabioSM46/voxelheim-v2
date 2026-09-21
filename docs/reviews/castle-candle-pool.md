# Castle candle renderer — issue #1204

The renderer consumes the existing authoritative static-prop roots for wall sconces,
floor candelabra and table candelabra. It adds shared iron/wax meshes and small
emissive flames as children, so complete-set snapshot replacement, instance entry
and world reset retain the static-prop owner's lifetime semantics.

The point-light pool admits at most eight fixtures within 24 blocks of the camera.
Ranking runs at most five times per second, with at most 32 shape-aware visibility
rays, stable ID ordering and a 1.5-block retention bias. Removed or hidden roots,
distance limits and reduced device capacity take effect immediately. Each fixture
uses one point light at its wick group, irrespective of its number of arms.

The cap also respects the actual render-device texture-array limit: each point
shadow cube occupies six layers, and existing shadowed point lights reserve their
layers first. Missing device information admits no shadowed candle lights. Meshes
and emissive flames remain visible even when their fixture does not receive a light.

The production sun uses two built-in shadow cascades over 64 blocks with a 2048
map. Point shadow maps use 512 pixels. This changes shadow reception, not the
existing celestial direction, brightness curve, weather or ambient-light policy.

Tests cover pool capacity changes, root removal, session exit, shared mesh reuse,
recursive teardown, production snapshot synchronization, rotated camera frusta,
and grille-member visibility with unloaded chunks failing closed.

No fixture catalogue is activated in this renderer part. The window/placement and
capture acceptance follow-up must tune the provisional fixture lumens and record
matched day/night views, active/shadowed light counts, draw submissions and frame
distributions through the production castle capture harness.
