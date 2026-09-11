# ADR 0003 — The client takes `arboard` for the system clipboard

- **Status**: accepted
- **Date**: 2026-09-11
- **Issue**: #1124
- **Decides for**: #1124 (Ctrl+C / Ctrl+X / Ctrl+V in text fields), #1132 (copying from the chat log)
- **Approved by**: the repository owner, in the feature-spec of 2026-09-11

The form is the one ADR 0001 set: what was decided, what it was decided against, and the
evidence. It is not updated when the code moves; a later ADR supersedes it.

## Decision

**The client gains a sixth crate, `arboard` 3.6.1, taken `default-features = false` with no
features enabled.** It is reached only through the `Clipboard` trait in
`client/src/ui/clipboard.rs`, whose game implementation opens the system clipboard lazily. Tests
use an in-memory implementation in the same build.

## Context

A player typing in chat wants to paste a coordinate or a name instead of retyping it. The client
has had text fields since chat existed, and #1124 gives them a cursor and a selection. The
clipboard is what that editing model cannot reach on its own: it is shared with every other
program on the desktop, and on X11 it is a protocol, not a buffer.

`client/AGENTS.md` asks for a discussion before a sixth crate. This is that discussion.

## Alternatives weighed

### winit, which the client already has

**winit has no clipboard API.** Every `clipboard` match in winit 0.30.13's source is a comment or
an unrelated platform detail, and no type in its public surface reads or writes one. Bevy's
window layer adds nothing either: `bevy_winit` exposes windows and input, not selections.

### Shelling out to `xclip`, `xsel` or `wl-paste` — rejected

This costs no crate, and it is rejected on the same ground the rustls decision of 2026-08-20
gave: a feature that works only when the host happens to be set up correctly silently does not
work.

- **None of those binaries is installed by default.** On a desktop without them, paste does
  nothing, and the only way to find out why is to read a log.
- **The choice depends on the session.** `xclip` on X11, `wl-copy` on Wayland, and a process
  spawn to find out which one is present, on every paste.
- **Copying on X11 needs a living owner.** The program that copied has to answer every later
  paste request. `xclip` does that by forking a background process per copy, so every copy
  leaves one behind.

### Bevy's own `system_clipboard` feature — closest, and rejected

This alternative was not in the feature-spec, and it is recorded here because it is the one that
looks as if it avoids the sixth crate. Bevy 0.19.1 has a `bevy_clipboard` crate. Its
`system_clipboard` feature is **the same `arboard` with `default-features = false`**, behind a
`Clipboard` resource. `client/AGENTS.md` says that adding a Bevy feature is not adding a
dependency, so on the letter of that rule it would cost nothing.

It is rejected for three reasons, and the first alone decides it.

1. **The backend is chosen at compile time.** `bevy_clipboard::Clipboard` is one concrete type
   whose fields are `cfg`-selected by the feature. With the feature on, every test in the build
   talks to the real clipboard, and with it off, the game does not. #1124 requires an in-memory
   backend for tests *and* the system backend for the game in the same binary. That needs a
   seam this client owns, and once the client owns the seam, the resource in front of `arboard`
   adds nothing.
2. **The graph cost is the same, so the rule's letter would be gamed.** The dependency budget
   exists because of what the graph costs, and `arboard` is in the graph either way. Enabling it
   through a Bevy feature would hide the sixth crate rather than avoid it, and the manifest is
   where it should be seen.
3. **It turns on more than a clipboard, and it discards the reason a clipboard failed.**
   `bevy/system_clipboard` also enables `bevy_text/system_clipboard`, the clipboard integration
   of Bevy's `EditableText`, which nothing here uses. Its resource also builds `arboard` at plugin
   initialisation with `.ok()`: the construction error is dropped and never retried, and every
   later call answers `ClipboardNotSupported` without saying why.

## The crate

**Chosen: `arboard` 3.6.1, `default-features = false`.** It is maintained, it is what Bevy itself
chose for the same job, and on Linux it speaks X11 through `x11rb`, a pure-Rust protocol crate.

**Measured on the pinned toolchain with `cargo tree -e normal --target x86_64-unknown-linux-gnu`**:
the graph grows by **one** package, `arboard` itself (312 to 313; 322 to 323 with build
dependencies). Its four dependencies on this target (`x11rb` 0.13, `log`, `parking_lot` and
`percent-encoding`) were already in the graph through winit. `Cargo.lock` gains six entries:
`arboard`, plus five that are built only for Windows (`clipboard-win`, `error-code`) and macOS
(`objc2-app-kit`, `objc2-core-graphics`, `objc2-io-surface`).

**No system dependency.** `cargo metadata` lists the same two `links` keys before and after,
`alsa-sys` and `ring`. `x11rb` without its `dl-libxcb` feature opens the X11 socket itself and
links nothing, so the CI apt list does not change.

### The feature set, and why it is empty

- **No `image-data`**, `arboard`'s default. It pulls the `image` crate and its codecs to copy
  pictures, and a text field copies text. `client/Cargo.toml` already keeps image codecs out of
  Bevy's feature list for the same reason.
- **No `wayland-data-control`.** It adds `wl-clipboard-rs` and a Wayland client stack. The
  client's window is X11 by design: `x11` and not `wayland` in the Bevy feature list, because
  `wayland` needs `libwayland-dev` at build time. On a Wayland desktop the client runs through
  XWayland, and XWayland bridges the X11 clipboard to the Wayland one. An X11 clipboard is
  therefore the one that matches the window the player is typing into. If the client ever takes
  Bevy's `wayland` feature, this feature is the one to revisit with it.

## Consequences

- `client/AGENTS.md` names six dependencies and points here. `client/Cargo.toml` carries the
  entry and its reasoning.
- **Every clipboard use goes through `ui/clipboard.rs`.** A text field never names `arboard`;
  #1132's log copy uses the same `TextClipboard`.
- **Nothing connects to a display until a shortcut is pressed.** The resource is installed in
  every app, including every headless test, and the connection is made lazily. That is what
  keeps CI, which has no display, from ever opening one.
- **Failures never panic, and each kind is logged once.** No display server and an empty
  clipboard are different kinds, and a paste that fails leaves the field untouched and working.
  A cut whose copy failed deletes nothing.
- **A paste blocks the frame until the selection owner answers.** On X11 a paste is a request
  to whichever program last copied. `arboard` waits for the reply on the calling thread for up
  to four seconds (`LONG_TIMEOUT_DUR`). An owner that answers promptly, which is every ordinary
  one, costs nothing visible. A hung one costs a stall, once, on the key press that asked. Moving
  the read off the main thread is the fix if that is ever seen, and it is not taken now.
- **Copied text lives as long as the client does, unless a clipboard manager takes it.** The
  system backend stays open once opened for that reason, and `arboard` hands the contents to a
  clipboard manager when it is dropped at exit.

## What was not decided here

Clipboard use outside text fields, images on the clipboard, and the X11 primary selection
(middle-click paste). Each would be its own change. Images would also need `image-data`, which is
a dependency argument of its own.
