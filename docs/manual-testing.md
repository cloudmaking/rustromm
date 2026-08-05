# Manual testing checklist

Most of RustRomM is covered by `cargo test` — 57 offline tests, plus 8 that run
against a real server. This file lists what automation genuinely cannot reach,
so it's clear what still needs a human.

## Nothing automated touches these

| Area | Why not |
|---|---|
| **Reading a physical controller** | Needs real hardware. The button-to-action mapping is a pure function with unit tests, and the keyboard path shares every downstream behaviour, but the `gilrs` polling layer is unverified by tests. |
| **Launching an emulator** | Spawning RetroArch is tested only as far as "the command was parsed correctly". Whether it actually opens the ROM depends on your setup. |
| **How anything looks** | A test can assert a label exists; it can't tell you the text is a row of `□` because the font lacks that glyph. That bug shipped once — see the v0.2.0 changelog. |
| **macOS and Windows behaviour** | CI compiles and runs the suite on both, but no test opens a window, and the binaries are unsigned. |

## Controller check

The one that matters most.

1. Plug a pad in. The hint bar bottom-right should switch from the keyboard
   hints to **"D-pad move · A download/play · B back · LB/RB console"**.
   - If it stays on the keyboard text, `gilrs` isn't seeing the pad. That's the
     first thing to debug — everything downstream is shared with the keyboard
     path, which is tested.
2. D-pad up/down moves the highlight, and the list scrolls to keep it visible.
3. Left stick does the same, once per push — holding it should not scroll
   continuously.
4. **A** on a highlighted game starts a download; **A** again once finished
   launches it.
5. **B** clears the search box, then the highlight.
6. **LB/RB** move between consoles in the sidebar.
7. Go to another console and back — the highlight should return to the game you
   were on, not the top of the list.
8. Leave the app idle with the pad connected, then press a button without
   touching the mouse. It should respond immediately.

## First-run check

1. Launch with no config. The connect screen should appear.
2. Enter a server as bare `host:port` — no `http://`. It should still connect.
3. Wrong password should say the password was rejected, not that the server is
   unreachable.
4. Reopen the app. With "remember password" ticked it should go straight to the
   library; unticked, back to the connect screen.

## Download check

1. Download a large game and watch the progress bar move.
2. Cancel midway. No file and no `.part` should be left in the download folder.
3. Download a multi-disc game — it should save as `.zip`.
4. Pull the network cable mid-download. The error should name the problem.

## macOS and Windows

Both binaries are unsigned and un-notarized, so first launch is blocked.

**macOS** shows *"Apple could not verify 'rustromm' is free of malware."* The fix
is to clear the download quarantine flag:

```sh
xattr -c rustromm && chmod +x rustromm && ./rustromm
```

The commonly-given "right-click → Open" advice does **not** work here, on two
counts: Apple removed that bypass in macOS Sequoia, and it only ever applied to
`.app` bundles, whereas each release is a bare executable. The GUI route is
System Settings → Privacy & Security → Open Anyway, after being blocked once.

**Windows**: SmartScreen → More info → Run anyway.

Once it starts, confirm the window is a sensible size, that text isn't clipped,
and that the font renders — the tofu bug in v0.2.0 was a font problem and could
recur on a platform with different bundled fonts.
