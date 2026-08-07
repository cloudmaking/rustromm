//! egui front-end and all UI state.
//!
//! Threading model: every network call runs on a detached `std::thread` and
//! reports back through an mpsc channel, followed by `ctx.request_repaint()`.
//! The UI thread never blocks on IO, and there is no async runtime to carry.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::api::{Api, PAGE_SIZE};
use crate::config::Config;
use crate::input::{Gamepads, NavAction, key_to_action};
use crate::launch;
use crate::libretro::cores::{self, NoCore};
use crate::libretro::emu::Emulator;
use crate::logging;
use crate::models::{Page, Platform, Rom, human_size};
use crate::play;

#[derive(PartialEq, Eq, Clone, Copy)]
enum Screen {
    Connect,
    Library,
    Settings,
    Logs,
    Play,
}

/// Results coming back from worker threads.
enum Msg {
    Connected(Api, String),
    ConnectFailed(String),
    Platforms(Vec<Platform>),
    Roms {
        page: Page<Rom>,
        offset: i64,
    },
    Failed(String),
    Cover(i64, Option<Arc<[u8]>>),
    Progress(i64, u64, Option<u64>),
    Downloaded(i64, PathBuf),
    DownloadFailed(i64, String),
    /// A core has been fetched and the ROM read; ready to start emulating.
    GameReady {
        core_path: PathBuf,
        rom_path: PathBuf,
        rom: Vec<u8>,
        title: String,
    },
    GameFailed(String),
}

struct Download {
    done: u64,
    total: Option<u64>,
    cancel: Arc<AtomicBool>,
    finished: Option<PathBuf>,
    error: Option<String>,
}

impl Download {
    fn fraction(&self) -> Option<f32> {
        match self.total {
            Some(t) if t > 0 => Some((self.done as f32 / t as f32).clamp(0.0, 1.0)),
            _ => None,
        }
    }
}

pub struct RustRomm {
    config: Config,
    api: Option<Api>,
    screen: Screen,

    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    // Connect screen
    connecting: bool,
    connect_error: Option<String>,
    server_version: Option<String>,

    // Library
    platforms: Vec<Platform>,
    selected_platform: Option<i64>,
    roms: Vec<Rom>,
    total_roms: i64,
    offset: i64,
    search: String,
    /// The search text that produced the currently displayed page. Used to tell
    /// "user is mid-typing" from "results are stale and need refetching".
    applied_search: String,
    loading: bool,
    error: Option<String>,

    covers: HashMap<i64, Option<Arc<[u8]>>>,
    cover_requested: HashSet<i64>,

    downloads: HashMap<i64, Download>,
    status: Option<String>,

    /// Highlighted row, as an index into `roms`. Drives keyboard and
    /// controller navigation; `None` means nothing is highlighted yet.
    selected: Option<usize>,
    /// Scroll the highlighted row into view on the next frame. Set when the
    /// selection moves by key or pad, so the list follows the highlight.
    scroll_to_selected: bool,
    /// Where the highlight was, per platform. Coming back to a console
    /// returns you to the game you were on rather than the top of the list —
    /// without this, controller browsing means re-scrolling every time.
    remembered_selection: HashMap<Option<i64>, usize>,
    gamepads: Gamepads,

    /// Platform slug whose emulator field should be highlighted in Settings.
    /// Set when a launch is attempted with nothing configured, so the user
    /// lands on the exact field that needs filling in rather than a wall of them.
    needs_emulator_for: Option<String>,

    // Embedded emulation
    /// The running game, if any. Dropping it stops the emulation thread and
    /// unloads the core.
    emulator: Option<Emulator>,
    /// The texture the game is drawn into. Reused across frames — allocating a
    /// new one every frame leaks GPU memory until the driver gives up.
    game_texture: Option<egui::TextureHandle>,
    game_title: String,
    /// Set while a core is downloading, so Play does not look like it did
    /// nothing during the first-run fetch.
    preparing: Option<String>,
    /// The sound device, opened once and kept for the life of the app.
    ///
    /// Opening it per game would add an audible gap at the start of every one,
    /// and on some drivers repeated open/close leaks. `None` means no sound
    /// card, which must not stop a game running — a silent game beats a game
    /// that refuses to start, and CI has no sound card at all.
    audio: Option<crate::libretro::audio::AudioOutput>,
}

impl RustRomm {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::with_config(&cc.egui_ctx, Config::load())
    }

    /// Construct with an explicit config and bare `egui::Context`.
    ///
    /// Split out from `new` so tests can build the app without an
    /// `eframe::CreationContext`, which can't be constructed outside eframe.
    pub fn with_config(egui_ctx: &egui::Context, config: Config) -> Self {
        let (tx, rx) = channel();

        // Slightly roomier than egui's default; this is a browsing app.
        // `all_styles_mut` applies to both the light and dark themes.
        egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
            style.spacing.button_padding = egui::vec2(10.0, 6.0);
        });

        let mut app = Self {
            config,
            api: None,
            screen: Screen::Connect,
            tx,
            rx,
            connecting: false,
            connect_error: None,
            server_version: None,
            platforms: Vec::new(),
            selected_platform: None,
            roms: Vec::new(),
            total_roms: 0,
            offset: 0,
            search: String::new(),
            applied_search: String::new(),
            loading: false,
            error: None,
            covers: HashMap::new(),
            cover_requested: HashSet::new(),
            downloads: HashMap::new(),
            status: None,
            selected: None,
            scroll_to_selected: false,
            remembered_selection: HashMap::new(),
            gamepads: Gamepads::new(),
            needs_emulator_for: None,
            emulator: None,
            game_texture: None,
            game_title: String::new(),
            preparing: None,
            audio: None,
        };

        // Saved credentials mean we can go straight to the library.
        if !app.config.server_url.is_empty()
            && !app.config.username.is_empty()
            && !app.config.password.is_empty()
        {
            app.start_connect(egui_ctx);
        }
        app
    }

    /// Which screen is showing. Exposed for tests.
    pub fn on_connect_screen(&self) -> bool {
        self.screen == Screen::Connect
    }

    pub fn on_library_screen(&self) -> bool {
        self.screen == Screen::Library
    }

    /// Number of games in the currently displayed page. Exposed for tests.
    pub fn visible_rom_count(&self) -> usize {
        self.roms.len()
    }

    pub fn total_rom_count(&self) -> i64 {
        self.total_roms
    }

    pub fn platform_count(&self) -> usize {
        self.platforms.len()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.error.as_deref().or(self.connect_error.as_deref())
    }

    /// Block until every queued background message has been folded into state,
    /// or `timeout` elapses. Tests need this because the worker threads are
    /// genuinely asynchronous; production code just drains what has arrived.
    pub fn pump_until<F>(
        &mut self,
        ctx: &egui::Context,
        timeout: std::time::Duration,
        done: F,
    ) -> bool
    where
        F: Fn(&Self) -> bool,
    {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.drain_messages(ctx);
            if done(self) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Render one frame. Same path `eframe::App::ui` takes.
    pub fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.drain_messages(&ctx);
        match self.screen {
            Screen::Connect => self.connect_screen(ui),
            Screen::Library => self.library_screen(ui),
            Screen::Settings => self.settings_screen(ui),
            Screen::Logs => self.logs_screen(ui),
            Screen::Play => self.play_screen(ui),
        }
    }

    /// Kick off a connection using the current config. Exposed for tests.
    pub fn connect(&mut self, ctx: &egui::Context) {
        self.start_connect(ctx);
    }

    /// Index of the highlighted game, if any. Exposed for tests.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// True when a controller is plugged in.
    pub fn gamepad_connected(&self) -> bool {
        self.gamepads.any_connected()
    }

    /// Navigation intents from the keyboard and any connected controller.
    fn nav_actions(&mut self, ctx: &egui::Context) -> Vec<NavAction> {
        let mut actions = self.gamepads.poll();

        // Key navigation is suppressed while a text field has focus, or
        // typing "j" into the search box would jump down the list instead of
        // writing a letter.
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing {
            ctx.input(|i| {
                for event in &i.events {
                    // Held keys repeat, which is what you want when scrolling
                    // a long list.
                    if let egui::Event::Key {
                        key, pressed: true, ..
                    } = event
                        && let Some(action) = key_to_action(*key)
                    {
                        actions.push(action);
                    }
                }
            });
        }
        actions
    }

    /// Move the highlight, clamped to the current page.
    fn move_selection(&mut self, delta: isize) {
        if self.roms.is_empty() {
            self.selected = None;
            return;
        }
        let last = (self.roms.len() - 1) as isize;
        let next = match self.selected {
            // First press highlights an end of the list rather than jumping
            // to an arbitrary middle.
            None if delta > 0 => 0,
            None => last,
            Some(current) => (current as isize + delta).clamp(0, last),
        };
        self.selected = Some(next as usize);
        self.scroll_to_selected = true;
        self.remember_selection();
    }

    fn remember_selection(&mut self) {
        if let Some(index) = self.selected {
            self.remembered_selection
                .insert(self.selected_platform, index);
        }
    }

    /// Step through the sidebar: "All games" followed by each platform.
    /// Returns true when the selection actually changed.
    fn cycle_platform(&mut self, delta: isize) -> bool {
        let mut ids: Vec<Option<i64>> = vec![None];
        ids.extend(self.platforms.iter().map(|p| Some(p.id)));

        let current = ids
            .iter()
            .position(|id| *id == self.selected_platform)
            .unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, ids.len() as isize - 1) as usize;

        if ids[next] == self.selected_platform {
            return false;
        }
        self.selected_platform = ids[next];
        true
    }
}

/// What a navigation pass decided to do, applied after the borrow ends.
#[derive(Default)]
struct NavOutcome {
    fetch: Option<i64>,
    download: Option<Rom>,
    launch: Option<Rom>,
}

impl RustRomm {
    /// Apply one frame's worth of navigation input.
    fn handle_nav(&mut self, ctx: &egui::Context) -> NavOutcome {
        let mut out = NavOutcome::default();

        for action in self.nav_actions(ctx) {
            match action {
                NavAction::Up => self.move_selection(-1),
                NavAction::Down => self.move_selection(1),
                NavAction::PagePrev => {
                    if self.offset > 0 {
                        out.fetch = Some((self.offset - PAGE_SIZE).max(0));
                    }
                }
                NavAction::PageNext => {
                    if self.offset + PAGE_SIZE < self.total_roms {
                        out.fetch = Some(self.offset + PAGE_SIZE);
                    }
                }
                NavAction::PlatformPrev => {
                    if self.cycle_platform(-1) {
                        out.fetch = Some(0);
                    }
                }
                NavAction::PlatformNext => {
                    if self.cycle_platform(1) {
                        out.fetch = Some(0);
                    }
                }
                NavAction::Confirm => {
                    let Some(rom) = self.selected.and_then(|i| self.roms.get(i)).cloned() else {
                        continue;
                    };
                    match self.downloads.get(&rom.id) {
                        // Already downloaded — confirm means play.
                        Some(d) if d.finished.is_some() => out.launch = Some(rom),
                        // Mid-download: ignore rather than starting a second one.
                        Some(d) if d.error.is_none() => {}
                        _ if rom.missing_from_fs => {
                            self.status = Some(format!("{} is missing on the server", rom.title()));
                        }
                        _ => out.download = Some(rom),
                    }
                }
                NavAction::Back => {
                    // Escape backs out one step at a time: first the search,
                    // then the highlight.
                    if !self.search.is_empty() {
                        self.search.clear();
                        out.fetch = Some(0);
                    } else {
                        self.selected = None;
                    }
                }
            }
        }
        out
    }

    fn start_connect(&mut self, ctx: &egui::Context) {
        self.connecting = true;
        self.connect_error = None;

        let (url, user, pass) = (
            self.config.server_url.clone(),
            self.config.username.clone(),
            self.config.password.clone(),
        );
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let msg = match Api::new(&url, &user, &pass) {
                Ok(api) => match api.check_connection() {
                    Ok(version) => Msg::Connected(api, version),
                    Err(e) => Msg::ConnectFailed(format!("{e:#}")),
                },
                Err(e) => Msg::ConnectFailed(format!("{e:#}")),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn fetch_platforms(&self, ctx: &egui::Context) {
        let Some(api) = self.api.clone() else { return };
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let msg = match api.platforms() {
                Ok(mut p) => {
                    // Empty platforms are noise in the sidebar.
                    p.retain(|x| x.rom_count > 0);
                    p.sort_by(|a, b| {
                        a.display_name()
                            .to_lowercase()
                            .cmp(&b.display_name().to_lowercase())
                    });
                    Msg::Platforms(p)
                }
                Err(e) => Msg::Failed(format!("{e:#}")),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn fetch_roms(&mut self, ctx: &egui::Context, offset: i64) {
        let Some(api) = self.api.clone() else { return };
        self.loading = true;
        self.error = None;
        self.applied_search = self.search.clone();

        let platform = self.selected_platform;
        let search = self.search.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let msg = match api.roms(platform, &search, offset) {
                Ok(page) => Msg::Roms { page, offset },
                Err(e) => Msg::Failed(format!("{e:#}")),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn request_cover(&mut self, ctx: &egui::Context, rom: &Rom) {
        if self.cover_requested.contains(&rom.id) {
            return;
        }
        let Some(path) = rom.cover_path().map(str::to_string) else {
            self.cover_requested.insert(rom.id);
            self.covers.insert(rom.id, None);
            return;
        };
        let Some(api) = self.api.clone() else { return };
        self.cover_requested.insert(rom.id);

        let id = rom.id;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let bytes = api.cover(&path).map(|b| Arc::from(b.into_boxed_slice()));
            let _ = tx.send(Msg::Cover(id, bytes));
            ctx.request_repaint();
        });
    }

    fn start_download(&mut self, ctx: &egui::Context, rom: &Rom) {
        let Some(api) = self.api.clone() else { return };
        let dir = self.config.resolved_download_dir().join(&rom.platform_slug);
        let dest = dir.join(rom.download_file_name());

        let cancel = Arc::new(AtomicBool::new(false));
        self.downloads.insert(
            rom.id,
            Download {
                done: 0,
                total: None,
                cancel: cancel.clone(),
                finished: None,
                error: None,
            },
        );

        let rom = rom.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let id = rom.id;
            // Throttle progress messages: a 128 KB read loop on a fast local
            // server would otherwise flood the channel and repaint constantly.
            let mut last_sent = 0u64;
            let result = api.download_rom(&rom, &dest, &cancel, |done, total| {
                if done - last_sent >= 512 * 1024 || Some(done) == total {
                    last_sent = done;
                    let _ = tx.send(Msg::Progress(id, done, total));
                    ctx.request_repaint();
                }
            });
            let msg = match result {
                Ok(()) => Msg::Downloaded(id, dest),
                Err(e) => Msg::DownloadFailed(id, format!("{e:#}")),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn drain_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Connected(api, version) => {
                    logging::info(format!("connected to RomM {version} at {}", api.base_url()));
                    self.connecting = false;
                    self.api = Some(api);
                    self.server_version = Some(version);
                    self.screen = Screen::Library;
                    if let Err(e) = self.config.save() {
                        self.status =
                            Some(format!("Connected, but settings could not be saved: {e:#}"));
                    }
                    self.fetch_platforms(ctx);
                    self.fetch_roms(ctx, 0);
                }
                Msg::ConnectFailed(e) => {
                    logging::error(format!("connect failed: {e}"));
                    self.connecting = false;
                    self.connect_error = Some(e);
                }
                Msg::Platforms(p) => self.platforms = p,
                Msg::Roms { page, offset } => {
                    self.loading = false;
                    self.total_roms = page.total;
                    self.roms = page.items;
                    self.offset = offset;

                    // Put the highlight back where it was on this platform, so
                    // returning to a console doesn't dump you at the top of a
                    // thousand-game list. Clamped, because the page may be
                    // shorter than the one we remembered.
                    self.selected = self
                        .remembered_selection
                        .get(&self.selected_platform)
                        .copied()
                        .filter(|_| !self.roms.is_empty())
                        .map(|i| i.min(self.roms.len() - 1));
                    self.scroll_to_selected = self.selected.is_some();
                }
                Msg::Failed(e) => {
                    logging::error(format!("request failed: {e}"));
                    self.loading = false;
                    self.error = Some(e);
                }
                Msg::Cover(id, bytes) => {
                    self.covers.insert(id, bytes);
                }
                Msg::Progress(id, done, total) => {
                    if let Some(d) = self.downloads.get_mut(&id) {
                        d.done = done;
                        d.total = total;
                    }
                }
                Msg::Downloaded(id, path) => {
                    logging::info(format!("download complete: {}", path.display()));
                    if let Some(d) = self.downloads.get_mut(&id) {
                        d.finished = Some(path.clone());
                    }
                    self.status = Some(format!("Saved to {}", path.display()));
                }
                Msg::GameReady {
                    core_path,
                    rom_path,
                    rom,
                    title,
                } => {
                    self.preparing = None;
                    let dirs = self.config.system_dir();
                    // Opened lazily: a user who never plays anything never
                    // touches the sound device.
                    if self.audio.is_none() {
                        match crate::libretro::audio::AudioOutput::open() {
                            Ok(out) => self.audio = Some(out),
                            Err(e) => logging::warn(format!(
                                "no audio output ({e:#}) — the game will run silently"
                            )),
                        }
                    }
                    let sink = self.audio.as_ref().map(|a| Arc::clone(&a.buffer));
                    match Emulator::start(core_path, rom_path, rom, dirs.clone(), dirs, sink) {
                        Ok(emu) => {
                            logging::info(format!(
                                "playing {title} on {} {}",
                                emu.info.name, emu.info.version
                            ));
                            self.game_title = title;
                            self.game_texture = None;
                            self.emulator = Some(emu);
                            self.screen = Screen::Play;
                        }
                        Err(e) => {
                            logging::error(format!("could not start {title}: {e:#}"));
                            self.status = Some(format!("{e:#}"));
                        }
                    }
                }
                Msg::GameFailed(e) => {
                    self.preparing = None;
                    logging::error(format!("could not prepare game: {e}"));
                    self.status = Some(e);
                }
                Msg::DownloadFailed(id, e) => {
                    logging::error(format!("download {id} failed: {e}"));
                    if let Some(d) = self.downloads.get_mut(&id) {
                        d.error = Some(e.clone());
                    }
                    self.status = Some(format!("Download failed: {e}"));
                }
            }
        }
    }
}

impl RustRomm {
    /// Play a downloaded ROM.
    ///
    /// Embedded emulation is the default and the point of the app. The external
    /// launcher survives only for platforms with no embedded core — PSP, N64,
    /// arcade — where standalone emulators are better anyway and we deliberately
    /// refuse the hardware rendering they need.
    fn start_playing(&mut self, ctx: &egui::Context, rom: &Rom, path: PathBuf) {
        let spec = match cores::core_for_platform(&rom.platform_slug) {
            Ok(spec) => spec,
            Err(reason) => return self.launch_externally(rom, path, reason),
        };

        // BIOS is checked here, before the core is ever called, because a
        // missing one is not reliably a clean refusal — Handy segfaults inside
        // retro_load_game without lynxboot.img, and by then the process is gone.
        let system_dir = self.config.system_dir();
        let _ = std::fs::create_dir_all(&system_dir);
        let missing = cores::missing_bios(&spec, &system_dir);
        if !missing.is_empty() {
            let msg = format!(
                "{} needs {} in {} before it can run. Put the file there and try again.",
                rom.platform_display_name,
                missing.join(" and "),
                system_dir.display()
            );
            logging::warn(format!(
                "refusing to load {} — missing BIOS {:?}; some cores crash rather than \
                 reporting this",
                spec.name, missing
            ));
            self.status = Some(msg);
            return;
        }

        self.preparing = Some(format!("Preparing {}…", spec.display));
        self.status = None;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let cores_dir = self.config.cores_dir();
        let title = rom.title().to_string();
        std::thread::spawn(move || {
            let client = reqwest::blocking::Client::new();
            let msg = (|| -> anyhow::Result<Msg> {
                let core_path = cores::ensure_core(&client, &cores_dir, spec.name)?;
                let bytes = std::fs::read(&path)
                    .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))?;
                Ok(Msg::GameReady {
                    core_path,
                    rom_path: path,
                    rom: bytes,
                    title,
                })
            })()
            .unwrap_or_else(|e| Msg::GameFailed(format!("{e:#}")));
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    /// The old launcher path, now reached only where no embedded core exists.
    fn launch_externally(&mut self, rom: &Rom, path: PathBuf, reason: NoCore) {
        match self.config.emulator_for(&rom.platform_slug) {
            Some(cmd) => {
                logging::info(format!("launching {} via `{cmd}`", rom.title()));
                self.status = Some(match launch::launch(cmd, &path) {
                    Ok(()) => format!("Launched {} in your emulator", rom.title()),
                    Err(e) => {
                        logging::error(format!("launch failed: {e:#}"));
                        format!("{e:#}")
                    }
                });
            }
            None => {
                // Says *why* there is no embedded core, rather than a bare
                // "configure an emulator". The distinction is real and
                // permanent: PSP needs a GPU core we deliberately never
                // provide, so no future version will play it in-app.
                self.status = Some(format!("{} Set one up below.", reason.message()));
                logging::warn(format!(
                    "no embedded core for '{}': {}",
                    rom.platform_slug,
                    reason.message()
                ));
                self.needs_emulator_for = Some(rom.platform_slug.clone());
                self.screen = Screen::Settings;
            }
        }
    }

    /// Stop the running game and go back to the library.
    fn stop_playing(&mut self) {
        // Dropping the Emulator joins the emulation thread, which unloads the
        // core and releases the one-core-per-process slot. Without the join, the
        // next game would fail to start.
        self.emulator = None;
        self.game_texture = None;
        self.screen = Screen::Library;
    }

    fn play_screen(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let Some(emu) = self.emulator.as_ref() else {
            self.screen = Screen::Library;
            return;
        };

        // The emulation thread runs on its own clock and produces no window
        // events, so without an explicit repaint request the picture freezes
        // until the mouse moves.
        ctx.request_repaint();

        if emu.finished() {
            logging::info("game ended");
            self.stop_playing();
            return;
        }

        // Read input before drawing, so the newest press reaches the core as
        // early as possible.
        let mut mask = ctx.input(|i| play::retropad_from_keys(|k| i.key_down(k)));
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        #[cfg(feature = "gamepad")]
        {
            let (pad_mask, x, y) = self.gamepads.retropad_state();
            mask = play::apply_stick(mask | pad_mask, x, y);
        }
        emu.set_buttons(0, mask);

        let aspect = emu.av.aspect_ratio;
        let frame = emu.frame();
        let paused = emu.is_paused();
        let fps = emu.measured_fps();
        let target_fps = emu.av.fps;
        let core_name = emu.info.name.clone();

        egui::Panel::top("play_bar").show(root, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Stop").clicked() {
                    self.emulator = None;
                }
                if let Some(emu) = self.emulator.as_ref() {
                    if ui.button(if paused { "Resume" } else { "Pause" }).clicked() {
                        emu.set_paused(!paused);
                    }
                    if ui.button("Reset").clicked() {
                        emu.reset();
                    }
                }
                ui.separator();
                ui.label(&self.game_title);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Shown because a game running at 45 fps looks like a
                    // stutter and is impossible to report without a number.
                    let slow = fps > 1.0 && (fps as f64) < target_fps * 0.95;
                    let text = format!("{fps:.1} fps");
                    if slow {
                        ui.colored_label(egui::Color32::from_rgb(230, 160, 60), text);
                    } else {
                        ui.weak(text);
                    }
                    ui.weak(&core_name);
                });
            });
        });

        if self.emulator.is_none() {
            self.stop_playing();
            return;
        }
        if escape {
            self.stop_playing();
            return;
        }

        egui::CentralPanel::default_margins().show(root, |ui| {
            let Some(frame) = frame else {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
                return;
            };
            play::upload(&ctx, &mut self.game_texture, &frame);
            let Some(texture) = self.game_texture.as_ref() else {
                return;
            };

            let size = play::placement(&frame, aspect, ui.available_size());
            // Black around the game rather than the app background: it reads as
            // a screen bezel instead of a layout mistake.
            let rect = ui.available_rect_before_wrap();
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            ui.centered_and_justified(|ui| {
                ui.add(egui::Image::new(texture).fit_to_exact_size(size));
            });
        });
    }
}

impl eframe::App for RustRomm {
    // egui 0.35 hands the app a root `Ui` rather than the `Context`; panels are
    // nested inside it instead of being registered against the context.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl RustRomm {
    fn connect_screen(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let ctx = &ctx;
        egui::CentralPanel::default_margins().show(root, |ui| {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.heading("RustRomM");
                ui.label(egui::RichText::new("A desktop client for your RomM library").weak());
            });
            ui.add_space(24.0);

            // Keep the form to a readable column rather than stretching it.
            let width = 420.0_f32.min(ui.available_width() - 40.0);
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.label("Server address");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.config.server_url)
                                .hint_text("192.168.1.10:8087")
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(10.0);

                        ui.label("Username");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.config.username)
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(10.0);

                        ui.label("Password");
                        let pw = ui.add(
                            egui::TextEdit::singleline(&mut self.config.password)
                                .password(true)
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(6.0);
                        ui.checkbox(&mut self.config.remember_password, "Remember password");

                        ui.add_space(16.0);
                        let submitted =
                            pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let clicked = ui
                            .add_enabled(
                                !self.connecting,
                                egui::Button::new(if self.connecting {
                                    "Connecting…"
                                } else {
                                    "Connect"
                                })
                                .min_size(egui::vec2(width, 34.0)),
                            )
                            .clicked();

                        if (clicked || submitted) && !self.connecting {
                            self.start_connect(ctx);
                        }

                        if let Some(err) = &self.connect_error {
                            ui.add_space(12.0);
                            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), err);
                        }
                    },
                );
            });
        });
    }

    fn library_screen(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let ctx = &ctx;

        // Keyboard and controller input is resolved before any widget is
        // drawn, so a key press and a click take the same code path.
        let nav = self.handle_nav(ctx);

        // Actions are collected during the UI pass and applied afterwards, so
        // nothing mutates `self` while a widget still borrows part of it.
        let mut want_fetch: Option<i64> = nav.fetch;
        let mut to_download: Option<Rom> = nav.download;
        let mut to_launch: Option<(Rom, PathBuf)> = nav.launch.and_then(|rom| {
            self.downloads
                .get(&rom.id)
                .and_then(|d| d.finished.clone())
                .map(|path| (rom, path))
        });
        let mut to_reveal: Option<PathBuf> = None;
        let mut covers_needed: Vec<Rom> = Vec::new();

        // A connected pad produces no window events, so egui would otherwise
        // stop repainting and the app would look frozen until the mouse moved.
        if self.gamepads.any_connected() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

        egui::Panel::top("top").show(root, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("RustRomM");
                ui.separator();

                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search the library…")
                        .desired_width(260.0),
                );
                if search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    want_fetch = Some(0);
                }
                if ui.button("Search").clicked() {
                    want_fetch = Some(0);
                }
                if !self.search.is_empty() && ui.button("Clear").clicked() {
                    self.search.clear();
                    want_fetch = Some(0);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Settings").clicked() {
                        self.screen = Screen::Settings;
                    }
                    if ui.button("Logs").clicked() {
                        self.screen = Screen::Logs;
                    }
                    if let Some(v) = &self.server_version {
                        ui.label(egui::RichText::new(format!("RomM {v}")).weak());
                    }
                });
            });
            ui.add_space(4.0);
        });

        egui::Panel::left("platforms")
            .resizable(true)
            .default_size(210.0)
            .show(root, |ui| {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("PLATFORMS").small().strong());
                ui.add_space(4.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let all = self.selected_platform.is_none();
                    if ui
                        .selectable_label(all, format!("All games ({})", self.total_roms))
                        .clicked()
                        && !all
                    {
                        self.selected_platform = None;
                        want_fetch = Some(0);
                    }
                    ui.separator();
                    for p in &self.platforms {
                        let selected = self.selected_platform == Some(p.id);
                        let label = format!("{} ({})", p.display_name(), p.rom_count);
                        if ui.selectable_label(selected, label).clicked() && !selected {
                            self.selected_platform = Some(p.id);
                            want_fetch = Some(0);
                        }
                    }
                });
            });

        egui::Panel::bottom("status").show(root, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                let active = self
                    .downloads
                    .values()
                    .filter(|d| d.finished.is_none() && d.error.is_none())
                    .count();
                if active > 0 {
                    ui.spinner();
                    ui.label(format!("{active} download(s) in progress"));
                    ui.separator();
                }
                match &self.status {
                    Some(s) => {
                        ui.label(egui::RichText::new(s).weak());
                    }
                    None => {
                        ui.label(egui::RichText::new(format!("{} games", self.total_roms)).weak());
                    }
                }

                // Control hints, right-aligned. The pad version appears only
                // once one is plugged in — otherwise it's noise for mouse users.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let hint = if self.gamepads.any_connected() {
                        "D-pad move  ·  A download/play  ·  B back  ·  LB/RB console"
                    } else {
                        "Up/Down move  ·  Enter download/play  ·  Esc back  ·  PgUp/PgDn console"
                    };
                    ui.label(egui::RichText::new(hint).weak().small());
                });
            });
            ui.add_space(3.0);
        });

        egui::CentralPanel::default_margins().show(root, |ui| {
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), err);
                ui.separator();
            }
            if self.loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading…");
                });
                return;
            }
            if self.roms.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(if self.applied_search.is_empty() {
                        "No games here yet."
                    } else {
                        "Nothing matched that search."
                    });
                });
                return;
            }

            let scroll_now = self.scroll_to_selected;
            let selected = self.selected;
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (index, rom) in self.roms.iter().enumerate() {
                    let dl = self.downloads.get(&rom.id);
                    let is_selected = selected == Some(index);

                    // Highlight has to read from across a room, not just be a
                    // subtle outline — this is the cue you steer by on a pad.
                    let mut frame = egui::Frame::default().inner_margin(4.0);
                    if is_selected {
                        frame = frame
                            .fill(ui.visuals().selection.bg_fill.gamma_multiply(0.45))
                            .corner_radius(4.0);
                    }

                    let row = frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Cover
                            let size = egui::vec2(48.0, 64.0);
                            match self.covers.get(&rom.id) {
                                Some(Some(bytes)) => {
                                    ui.add_sized(
                                        size,
                                        egui::Image::from_bytes(
                                            format!("bytes://cover-{}", rom.id),
                                            bytes.clone(),
                                        )
                                        .maintain_aspect_ratio(true),
                                    );
                                }
                                Some(None) => {
                                    ui.allocate_space(size);
                                }
                                None => {
                                    covers_needed.push(rom.clone());
                                    ui.allocate_space(size);
                                }
                            }

                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(rom.title()).strong());
                                let mut meta = format!(
                                    "{} · {}",
                                    rom.platform_display_name,
                                    human_size(rom.fs_size_bytes)
                                );
                                if rom.missing_from_fs {
                                    meta.push_str(" · missing on server");
                                }
                                ui.label(egui::RichText::new(meta).weak().small());

                                if let Some(d) = dl {
                                    if let Some(err) = &d.error {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(220, 90, 90),
                                            egui::RichText::new(err).small(),
                                        );
                                    } else if d.finished.is_none() {
                                        let bar = match d.fraction() {
                                            Some(f) => egui::ProgressBar::new(f)
                                                .desired_width(240.0)
                                                .text(format!("{:.0}%", f * 100.0)),
                                            // No Content-Length: show motion, not a lie.
                                            None => egui::ProgressBar::new(0.0)
                                                .desired_width(240.0)
                                                .animate(true)
                                                .text(human_size(d.done as i64)),
                                        };
                                        ui.add(bar);
                                    }
                                }
                            });

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| match dl {
                                    Some(d) if d.finished.is_some() => {
                                        let path = d.finished.clone().unwrap();
                                        if ui.button("Play").clicked() {
                                            to_launch = Some((rom.clone(), path.clone()));
                                        }
                                        if ui.button("Folder").clicked() {
                                            to_reveal = Some(
                                                path.parent()
                                                    .map(Into::into)
                                                    .unwrap_or(path.clone()),
                                            );
                                        }
                                    }
                                    Some(d) if d.error.is_none() => {
                                        if ui.button("Cancel").clicked() {
                                            d.cancel.store(true, Ordering::Relaxed);
                                        }
                                    }
                                    _ => {
                                        if ui
                                            .add_enabled(
                                                !rom.missing_from_fs,
                                                egui::Button::new("Download"),
                                            )
                                            .clicked()
                                        {
                                            to_download = Some(rom.clone());
                                        }
                                    }
                                },
                            );
                        });
                    });

                    // Keep the highlight on screen when it moves by key or pad.
                    if is_selected && scroll_now {
                        row.response.scroll_to_me(Some(egui::Align::Center));
                    }
                    ui.separator();
                }
            });

            // Pagination
            if self.total_roms > PAGE_SIZE {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let page = self.offset / PAGE_SIZE + 1;
                    let pages = (self.total_roms + PAGE_SIZE - 1) / PAGE_SIZE;
                    if ui
                        .add_enabled(self.offset > 0, egui::Button::new("Previous"))
                        .clicked()
                    {
                        want_fetch = Some((self.offset - PAGE_SIZE).max(0));
                    }
                    ui.label(format!("Page {page} of {pages}"));
                    if ui
                        .add_enabled(
                            self.offset + PAGE_SIZE < self.total_roms,
                            egui::Button::new("Next"),
                        )
                        .clicked()
                    {
                        want_fetch = Some(self.offset + PAGE_SIZE);
                    }
                });
            }
        });

        // The scroll request is honoured for exactly one frame.
        self.scroll_to_selected = false;

        for rom in covers_needed {
            self.request_cover(ctx, &rom);
        }
        if let Some(rom) = to_download {
            self.start_download(ctx, &rom);
        }
        if let Some((rom, path)) = to_launch {
            self.start_playing(ctx, &rom, path);
        }
        if let Some(path) = to_reveal {
            if let Err(e) = launch::open_with_os(&path) {
                self.status = Some(format!("{e:#}"));
            }
        }
        if let Some(offset) = want_fetch {
            self.fetch_roms(ctx, offset);
        }
    }

    fn settings_screen(&mut self, root: &mut egui::Ui) {
        let mut save_now = false;
        egui::CentralPanel::default_margins().show(root, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Back").clicked() {
                    self.screen = Screen::Library;
                }
                ui.heading("Settings");
            });
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(egui::RichText::new("Downloads").strong());
                // Resolved before the edit box borrows `config` mutably.
                let default_dir = self.config.resolved_download_dir().display().to_string();
                ui.horizontal(|ui| {
                    ui.label("Folder:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.download_dir)
                            .hint_text(default_dir)
                            .desired_width(340.0),
                    );
                    if ui.button("Browse…").clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        self.config.download_dir = dir.display().to_string();
                        save_now = true;
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "Games are saved into a sub-folder per platform. Leave blank for the default.",
                    )
                    .weak()
                    .small(),
                );

                ui.add_space(16.0);
                ui.label(egui::RichText::new("Emulators").strong());
                ui.label(
                    egui::RichText::new(
                        "RustRomM doesn't emulate anything itself — it hands the file to an emulator you already have. \
                         Use {rom} where the file path should go; if you leave it out, the path is added at the end.",
                    )
                    .weak()
                    .small(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Default:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.default_emulator)
                            .hint_text("retroarch")
                            .desired_width(280.0),
                    );
                    if ui.button("Browse…").clicked()
                        && let Some(file) = rfd::FileDialog::new().pick_file()
                    {
                        self.config.default_emulator = file.display().to_string();
                        save_now = true;
                    }
                    if ui.button("Detect").clicked() {
                        let found = launch::detect_emulators();
                        logging::info(format!("emulator scan found {} candidate(s)", found.len()));
                        match found.first() {
                            Some((label, command)) => {
                                self.config.default_emulator = command.clone();
                                self.status = Some(format!("Found {label}."));
                                save_now = true;
                            }
                            None => {
                                self.status = Some(
                                    "No emulator found in the usual places — use Browse to \
                                     point at one."
                                        .to_string(),
                                );
                            }
                        }
                    }
                });

                ui.add_space(10.0);
                ui.label(egui::RichText::new("Per platform").small().strong());
                for p in &self.platforms {
                    let entry = self
                        .config
                        .emulators
                        .entry(p.slug.clone())
                        .or_default();
                    let wanted = self.needs_emulator_for.as_deref() == Some(p.slug.as_str());
                    ui.horizontal(|ui| {
                        let mut label = egui::RichText::new(p.display_name());
                        if wanted {
                            label = label.strong().color(egui::Color32::from_rgb(220, 170, 70));
                        }
                        ui.add_sized(egui::vec2(150.0, 20.0), egui::Label::new(label).truncate());
                        let field = ui.add(
                            egui::TextEdit::singleline(entry)
                                .hint_text("leave blank to use the default")
                                .desired_width(300.0),
                        );
                        // Land the cursor on the field that sent them here.
                        if wanted && !field.has_focus() {
                            field.request_focus();
                        }
                        if ui.button("Browse…").clicked()
                            && let Some(file) = rfd::FileDialog::new().pick_file()
                        {
                            *entry = file.display().to_string();
                            save_now = true;
                        }
                    });
                }

                ui.add_space(16.0);
                ui.label(egui::RichText::new("Account").strong());
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{} at {}",
                        self.config.username,
                        self.api.as_ref().map(Api::base_url).unwrap_or("—")
                    ));
                    if ui.button("Sign out").clicked() {
                        self.config.password.clear();
                        self.config.remember_password = false;
                        self.api = None;
                        self.roms.clear();
                        self.platforms.clear();
                        self.screen = Screen::Connect;
                        save_now = true;
                    }
                });

                ui.add_space(16.0);
                if ui.button("Save settings").clicked() {
                    save_now = true;
                }
                if let Ok(p) = Config::path() {
                    ui.label(
                        egui::RichText::new(format!("Config file: {}", p.display()))
                            .weak()
                            .small(),
                    );
                }
            });
        });

        if save_now {
            self.status = Some(match self.config.save() {
                Ok(()) => {
                    logging::info("settings saved");
                    "Settings saved.".to_string()
                }
                Err(e) => {
                    logging::error(format!("could not save settings: {e:#}"));
                    format!("Could not save settings: {e:#}")
                }
            });
        }
    }

    /// Everything the app has recorded this session, with a copy button.
    ///
    /// The point is that a user can hand over a diagnosis without opening a
    /// terminal. Failures that only reach stderr may as well not exist.
    fn logs_screen(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        egui::CentralPanel::default_margins().show(root, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Back").clicked() {
                    self.screen = Screen::Library;
                }
                ui.heading("Logs");

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Clear").clicked() {
                        logging::clear();
                    }
                    if ui.button("Copy all").clicked() {
                        ctx.copy_text(logging::report());
                        self.status = Some("Log copied to the clipboard.".to_string());
                    }
                });
            });
            ui.label(
                egui::RichText::new(
                    "Copy this and send it over if something misbehaves. It records what the \
                     app did, not what is in your library — no game data, no password.",
                )
                .weak()
                .small(),
            );
            ui.separator();

            let entries = logging::entries();
            if entries.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label("Nothing logged yet."));
                return;
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                // Newest entries matter most, and they are at the bottom.
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for entry in entries {
                        let colour = match entry.level {
                            logging::Level::Info => ui.visuals().weak_text_color(),
                            logging::Level::Warn => egui::Color32::from_rgb(220, 170, 70),
                            logging::Level::Error => egui::Color32::from_rgb(220, 90, 90),
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{:7.2}s", entry.at))
                                    .monospace()
                                    .weak()
                                    .small(),
                            );
                            ui.label(
                                egui::RichText::new(&entry.message)
                                    .monospace()
                                    .small()
                                    .color(colour),
                            );
                        });
                    }
                });
        });
    }
}
