# Changelog

## v0.2.0 — 2026-08-05

Controller navigation. The project exists because RomM's browser player handles
gamepads badly; launching an external emulator fixed playing, but picking a game
still meant reaching for a mouse. Now it doesn't.

- Browse entirely from a controller: D-pad or left stick moves, **A** downloads
  or plays, **B** backs out, shoulder buttons change console.
- The same actions on the keyboard: arrows or `j`/`k`, `Enter`, `Esc`,
  `PgUp`/`PgDn`.
- The highlight is remembered per console, so returning to a platform puts you
  back on the game you were looking at.
- Analog sticks latch, so holding one moves the highlight once rather than every
  frame, and a resting stick's drift is ignored.
- Controller support is a Cargo feature (`gamepad`), on by default. Build with
  `--no-default-features` if `gilrs` won't compile on your platform.

Fixed:

- The control hints rendered as tofu boxes. egui's bundled font has no arrow
  glyphs, so `↑↓`, `▶` and `‹ ›` all drew as `□`. Found by screenshotting the
  running app — no test could have caught it.
- egui only repaints on window events, and a controller produces none, so the
  app appeared frozen until the mouse moved.
- Typing `j` into the search box scrolled the list instead of writing a letter.

Tests: 57 offline (up from 40), plus 8 opt-in against a real server.

## v0.1.0 — 2026-08-05

First release. Connect to a RomM server, browse the library by platform with
cover art, search, download with progress and cancellation, and launch games in
an emulator you already have.

- Native builds for Linux, macOS and Windows, x86_64 and arm64. Every target
  except Intel macOS is compiled and tested on a runner of its own architecture.
- Downloads stream to a `.part` file and are renamed only on success, so an
  interrupted transfer never leaves a truncated ROM.
- Verified against RomM 5.1.0.
