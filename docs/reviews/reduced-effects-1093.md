# Reduced effects (#1093)

**Reduced effects** is a Graphics setting, off by default, for a player who finds the Draugr king's spell
flourishes uncomfortable or distracting. It removes decoration and never information, and it is presentation
only: nothing that predicts, sends or decides reads it.

**Withheld when it is on:**

- **The spell layer.** Burial's cracks, the Edict's runes and tally, and the Requiem's notes and closing ring,
  including the shards and spokes raised on a contact tick. Also the thrown Sepulchre Spear crystal.
- **The regalia.** The crystal forming in the king's hand over the Sepulchre Spear telegraph, and the chest
  core's brightening on a spell's contact tick.

**Drawn in both modes, on the same authoritative ticks:**

- **Hazard boundaries.** Dashed while announced, a continuous double line on contact.
- **Encounter readings.** Boss, stage, move, phase, progress, pulse count and whether the move is
  interruptible. The Edict's tally is withheld, but the pulse it named stays in the reading.
- **The body.** Every telegraph pose, the mask falling, the crown slipping, and the final stage's steady core
  light.
- **The Vargr, entirely**, including the strike reach layer, which is not an optional flourish.

The setting is stored as `reduced-effects on|off`. A settings file written before it existed loads it off.
Switching it mid-encounter takes effect on the next frame, with no new announcement.

## Reproduction

From `<repo-root>/client`:

```sh
cargo test --locked -- player::mobs::reduced_effects player::encounters settings ui::settings
cargo test --locked player::encounters::spells::capture::capture_reduced_effects -- --ignored --exact
```

The first includes the lockstep comparison: two headless clients receive identical snapshots and timelines,
one with the setting on. It also covers the switch made mid-channel and on a contact tick. The second needs a
GPU. It writes `reduced-1093-off-*.png` and `reduced-1093-on-*.png` to the temporary directory, from one
client that plays the same script twice and switches the setting between the two passes. Its sector
bearings, floor and player-sized box are review placements, not the server's sequence or a dungeon.

## Review record

Offscreen, 1280 x 720, default field of view, AMD Radeon RX 5700 XT with RADV and Vulkan: the inspection
adapter, not a frame-rate guarantee. Manual inspection on 2026-09-11:

- **Sepulchre Spear.** With the setting off, a pale crystal sits at the raised hand over the telegraph and a
  small crystal crosses the lane on release. With it on, neither is drawn. The pose, the "Casting 57%" and
  "ACTIVE 38%" readings, and the dashed lane that becomes a double line on release are the same in both
  modes.
- **Burial, Edict and Requiem.** With the setting on, the cracks, the rune circle with its staves, and the
  rings, spokes and raised shards are all gone. What stays in both modes, and reads without them:
  - the dashed boundaries while announced, and the double line on each contact tick;
  - the readings: "PULSE ACTIVE 93% - pulse 2/4", "Channeling 44% - pulse 2/3", and "PULSE ACTIVE 94% -
    pulse 3/3 [interruptible]".

  The Edict's pulse tally is gone with the runes, but the reading still names that pulse.
- **Core.** On the Requiem's contact tick the core flares near-white with the setting off. With it on, the
  core holds the final stage's steady light blue. The bare face, the slipped crown and the pose are the same.
- **13 blocks.** The spear telegraph looks the same in both modes: at that range the hand crystal was not
  distinguishable even with the setting off. The Requiem's contact sectors keep their double lines in both
  modes, and with the setting on they are the only mark on the floor.
- **25 blocks.** The spear lane and both readings stay legible. The Requiem's double line is thin, and the far
  sector's ring is marginal in both modes. With the setting off, the raised shards make that sector easier to
  find at this range. The reading still names the pulse and its contact.

Sheets, each pairing off (left) with on (right): [spear](reduced-effects-1093/spear.png),
[rituals](reduced-effects-1093/rituals.png), [core](reduced-effects-1093/core.png),
[13/25 blocks](reduced-effects-1093/distance.png).

## Limitations

- Only the Draugr king has flourishes this setting withholds today.
- At 25 blocks a Requiem sector's double line is marginal in both modes. Reduced effects does not make it
  weaker, but it removes the raised shards that make the sector easier to find with the setting off.
  Thickening boundaries at range would restyle an essential cue, which is out of scope here.
- No audio, camera-shake or other accessibility option is claimed, and nothing here restyles an effect.
- The recorded inspection covers the placements named above, not a live encounter in the shipped chamber.
