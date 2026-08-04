# Notes from Argosy

Working notes taken from [rommapp/argosy-launcher](https://github.com/rommapp/argosy-launcher)
(GPL-3.0, Kotlin, ~1,300 source files), the official Android client for RomM. RustRomM is
GPL-3.0 for this reason: it follows Argosy's design, and a derivative of GPL code must stay
GPL.

Read this before adding a feature — Argosy has almost certainly solved it already, and the
shape of its solution is worth copying even where the code isn't portable.

## What Argosy actually is

Not a thin client. It embeds **libretrodroid** (a libretro frontend) plus Google's **oboe**
audio library and a pile of C++, so it emulates in-process and manages its own cores. That
is the bulk of the repository and none of it is portable to a desktop Rust app.

The parts worth learning from are the data layer and the interaction model.

## Why RustRomM exists

RomM's browser player uses EmulatorJS, and gamepad input through the browser is unreliable
— lag, dropped inputs, remapping that doesn't persist. Argosy avoids this by playing
natively.

**On the desktop, the same problem is already solved by RetroArch and friends**, which have
mature controller handling. So RustRomM doesn't need Argosy's emulation layer at all: by
launching an external emulator it inherits good controller support for free. This is the
single most important design conclusion from reading Argosy — the hard part of Argosy is
the part we can skip.

## Data layer — `app/src/main/kotlin/com/nendo/argosy/data/remote/romm/`

Worth knowing what's there, roughly in the order we'd want it:

| File | What it handles | Ported? |
|---|---|---|
| `RomMApi.kt`, `RomMApiClient.kt` | Endpoint definitions and HTTP plumbing | ✅ `src/api.rs` |
| `RomMConnectionManager.kt` | Server reachability, credential validation | ✅ `check_connection` |
| `RomMLibrarySyncService.kt` | Paged library sync into a local database | ⬜ we page on demand instead |
| `RomMGameFileSync.kt` | Downloading game files | ✅ `download_rom` |
| `RomMSaveModels.kt` | Save and save-state models | ⬜ next |
| `DeviceAuthPoller.kt` | Device-code pairing (the QR/code flow) | ⬜ see below |
| `RomMCollectionSyncService.kt` | Collections | ⬜ |
| `RomMAchievementService.kt` | RetroAchievements | ⬜ |
| `RomMCapabilities.kt` | Feature detection by server version | ⬜ worth doing |

`RomMCapabilities` is the interesting one: it probes what the server supports rather than
assuming, which is how Argosy stays compatible across RomM versions. RustRomM currently
assumes RomM 5.x.

## Authentication

Argosy uses the **device-code pairing** flow — the endpoints are
`/api/auth/device/init`, `/api/auth/device/token`, and `/api/client-tokens/pair/{code}`.
You get a short code, approve it in RomM's web UI, and the client receives a revocable
token. No password is ever typed into the client.

RustRomM v1 uses HTTP Basic instead, which RomM's OpenAPI spec accepts on every data
endpoint. It's far less code and works everywhere. **Pairing is the better design though** —
a revocable per-device token beats a stored password — and is the most worthwhile thing to
port next.

## Interface and controller handling

`design-system-docs/` in the Argosy repo is a genuinely good reference, though it's written
for Jetpack Compose on Android TV:

- `01-focus-management.md` — focus restoration when returning to a screen, explicit focus
  targets on load
- `02-input-handling.md` — D-pad and gamepad key events, analog sticks
- `03-navigation.md` — preserving focus across screen changes
- `04-theming.md` — focus indicators: glow, scale, saturation
- `05-tv-compose-components.md` — TV list/carousel components

The transferable ideas, none of which need Android:

1. **Everything reachable without a pointer.** Every control has a focus state and a
   defined neighbour in each direction.
2. **Focus is restored, not reset.** Coming back from a game returns you to the tile you
   launched, not the top of the list.
3. **Focus is unmistakable.** Scale plus glow, not a subtle outline — it's read from across
   a room.
4. **A confirms, B goes back.** Consistently, everywhere.

RustRomM does not do gamepad UI navigation yet. egui supports keyboard focus natively;
adding a pad would mean [`gilrs`](https://crates.io/crates/gilrs) translating gamepad events
into `egui::Event::Key`. That is the natural v2 feature, and it's what would make RustRomM
usable on a TV.

## Deliberately not ported

- **libretrodroid / embedded emulation** — the entire reason it isn't needed is above.
- **Steam, Gitea/GitHub/GitLab and Play Store integrations** — Argosy also acts as a general
  Android launcher. Out of scope.
- **QuayPass / BLE** — Android-specific hardware pairing.
- **Music player** — RomM serves soundtracks; not what this tool is for.
