//! Keeps an open page current: reloads it when its file changes and starts
//! the math and Mermaid helpers for blocks that are not cached yet.
//!
//! [`WatchedPage`] wraps a [`DocPage`] and is what the windows show. All of
//! its work happens in [`Page::tick`], which the event loop calls while the
//! page asks for ticks; nothing here blocks, and nothing here opens a window.

use crate::{
    helpers::{self, HelperJob, HelperKind, Helpers, Locate},
    page::DocPage,
    paint::Canvas,
    window::Page,
};
use limpid_core::{doc::Mode, replies::source_hash, settings::Settings};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};
use winit::{
    event::{Ime, KeyEvent},
    keyboard::ModifiersState,
};

/// What the file looked like at the last look: modification time and length.
type Stamp = (SystemTime, u64);

fn stamp_of(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// How often to look at running helper children while any exist.
const HELPER_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Polls one file for changes, at most once per `interval`.
pub struct Watcher {
    path: PathBuf,
    interval: Duration,
    last_look: Option<Instant>,
    stamp: Option<Stamp>,
}

impl Watcher {
    /// A zero `interval` turns watching off.
    pub fn new(path: PathBuf, interval: Duration) -> Self {
        let stamp = stamp_of(&path);
        Self {
            path,
            interval,
            last_look: None,
            stamp,
        }
    }

    pub fn enabled(&self) -> bool {
        !self.interval.is_zero()
    }

    /// Whether the file changed since the last time this returned true (or
    /// since construction). A file that is briefly missing, as during an
    /// editor's save-by-rename, counts as unchanged until it reappears.
    pub fn changed(&mut self, now: Instant) -> bool {
        if !self.enabled() {
            return false;
        }
        if let Some(last) = self.last_look
            && now.saturating_duration_since(last) < self.interval
        {
            return false;
        }
        self.last_look = Some(now);
        let Some(current) = stamp_of(&self.path) else {
            return false;
        };
        if self.stamp == Some(current) {
            return false;
        }
        self.stamp = Some(current);
        true
    }
}

type CacheProbe = Box<dyn Fn(HelperKind, &str) -> bool>;

/// A [`DocPage`] that follows its file and keeps its caches filled.
pub struct WatchedPage {
    page: DocPage,
    file: PathBuf,
    watcher: Watcher,
    settings: Settings,
    helpers: Helpers,
    attempted: HashSet<String>,
    /// Jobs wait here while a child of the same kind is still running.
    deferred: Vec<HelperJob>,
    /// The source changed (or the page just opened): plan helpers again.
    replan: bool,
    /// How the page was parsed; helpers must see the same blocks.
    mode: Mode,
    /// Hash of the text most recently handed to the page to parse, while that
    /// parse runs. A file that changes back to what the page still shows must
    /// not be mistaken for "no change" while a newer text is on its way.
    requested: Option<String>,
    locate: Box<dyn Locate>,
    cached: CacheProbe,
}

impl WatchedPage {
    pub fn new(page: DocPage, file: PathBuf, settings: &Settings) -> Self {
        let mode = page.mode();
        let interval = Duration::from_millis(settings.view.watch_interval_ms);
        Self {
            page,
            watcher: Watcher::new(file.clone(), interval),
            file,
            settings: settings.clone(),
            helpers: Helpers::default(),
            attempted: HashSet::new(),
            deferred: Vec::new(),
            replan: true,
            mode,
            requested: None,
            locate: Box::new(helpers::locate_installed),
            cached: Box::new(helpers::is_cached),
        }
    }

    /// The mode the page was parsed in (agent pages use [`Mode::Agent`]).
    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Where helper programs are found; tests point this at fakes.
    pub fn with_locator(mut self, locate: impl Locate + 'static) -> Self {
        self.locate = Box::new(locate);
        self
    }

    /// How the cache is probed; tests use a fake cache.
    pub fn with_cache_probe(mut self, cached: impl Fn(HelperKind, &str) -> bool + 'static) -> Self {
        self.cached = Box::new(cached);
        self
    }

    pub fn page(&self) -> &DocPage {
        &self.page
    }

    pub fn page_mut(&mut self) -> &mut DocPage {
        &mut self.page
    }

    /// One step of work at time `now`. Returns whether the page changed.
    pub fn step(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self.watcher.changed(now) {
            changed |= self.reload_if_different();
        }
        // Helpers plan from the text on screen, so they wait for a parse in
        // flight to land.
        if self.replan && !self.page.parsing() {
            self.start_helpers();
        } else if !self.deferred.is_empty() {
            self.deferred = self.helpers.launch(std::mem::take(&mut self.deferred));
        }
        let ready = self.helpers.poll(now, &*self.cached);
        for failure in self.helpers.take_failures() {
            self.page.flash(failure.flash_text());
            changed = true;
        }
        if !ready.is_empty() {
            changed |= self.page.refresh_pending(&ready);
        }
        changed
    }

    /// Read the file again unless the page already shows what is on disk
    /// (its own saves land here too).
    fn reload_if_different(&mut self) -> bool {
        let Ok(bytes) = std::fs::read(&self.file) else {
            return false;
        };
        let hash = source_hash(&bytes);
        let showing = match &self.requested {
            Some(requested) if self.page.parsing() => requested.clone(),
            _ => source_hash(self.page.doc().source.as_bytes()),
        };
        if hash == showing {
            return false;
        }
        // One read, one parse (on the worker thread): the page gets the bytes
        // that were just read instead of reading the file again.
        self.page.reload_from_bytes(&bytes);
        self.requested = Some(hash);
        self.replan = true;
        true
    }

    fn start_helpers(&mut self) {
        self.replan = false;
        let jobs = helpers::plan(
            &self.page.doc().source,
            self.mode,
            &self.file,
            &self.settings,
            &self.attempted,
            &*self.locate,
            &*self.cached,
        );
        for job in &jobs {
            self.attempted.extend(job.hashes.iter().cloned());
        }
        let mut waiting = std::mem::take(&mut self.deferred);
        waiting.extend(jobs);
        self.deferred = self.helpers.launch(waiting);
    }
}

impl Page for WatchedPage {
    fn title(&self) -> String {
        self.page.title()
    }

    fn paint(&mut self, canvas: &mut Canvas, scale: f32) {
        self.page.paint(canvas, scale);
    }

    fn key(&mut self, event: &KeyEvent) -> bool {
        self.page.key(event)
    }

    fn modifiers_changed(&mut self, modifiers: ModifiersState) {
        self.page.modifiers_changed(modifiers);
    }

    fn ime(&mut self, event: &Ime) -> bool {
        self.page.ime(event)
    }

    fn closing(&self) -> bool {
        self.page.closing()
    }

    fn wants_ticks(&self) -> bool {
        self.page.wants_ticks()
            || self.watcher.enabled()
            || self.replan
            || self.helpers.busy()
            || !self.deferred.is_empty()
    }

    fn tick_period(&self) -> Duration {
        let working = self.replan || self.helpers.busy() || !self.deferred.is_empty();
        // Idle: wake only as often as the file is looked at.
        let own = if working {
            HELPER_POLL_INTERVAL
        } else {
            self.watcher.interval
        };
        if !self.page.wants_ticks() {
            return own;
        }
        // The page says how soon it needs a tick: every frame while an editor
        // box is open, the idle deadline when the editor only lingers.
        own.min(self.page.tick_period())
    }

    fn tick(&mut self) -> bool {
        let mut changed = self.page.tick();
        changed |= self.step(Instant::now());
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{paint::Canvas, text::Fonts, window::TICK_INTERVAL};
    use limpid_core::transport::Scheme;
    use std::{cell::RefCell, rc::Rc};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limpid-watch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write `text` and give the file a distinct modification time, since two
    /// writes in one test can land inside the filesystem's timestamp tick.
    fn write_at(path: &Path, text: &str, seconds: u64) {
        std::fs::write(path, text).unwrap();
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + seconds))
            .unwrap();
    }

    fn after(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    // Watcher.

    #[test]
    fn should_report_a_change_once_when_the_file_is_rewritten() {
        let dir = scratch("once");
        let path = dir.join("a.md");
        write_at(&path, "one\n", 1);
        let start = Instant::now();
        let mut watcher = Watcher::new(path.clone(), Duration::from_millis(10));
        assert!(!watcher.changed(after(start, 100)));
        write_at(&path, "two\n", 2);
        assert!(watcher.changed(after(start, 200)));
        assert!(
            !watcher.changed(after(start, 300)),
            "the change is reported once"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_notice_a_rewrite_of_the_same_length_when_the_time_differs() {
        let dir = scratch("same-length");
        let path = dir.join("a.md");
        write_at(&path, "one\n", 1);
        let start = Instant::now();
        let mut watcher = Watcher::new(path.clone(), Duration::from_millis(10));
        write_at(&path, "two\n", 2);
        assert!(watcher.changed(after(start, 100)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_not_look_again_when_the_interval_has_not_passed() {
        let dir = scratch("interval");
        let path = dir.join("a.md");
        write_at(&path, "one\n", 1);
        let start = Instant::now();
        let mut watcher = Watcher::new(path.clone(), Duration::from_millis(300));
        assert!(!watcher.changed(after(start, 0)));
        write_at(&path, "two\n", 2);
        assert!(!watcher.changed(after(start, 100)), "too soon to look");
        assert!(watcher.changed(after(start, 400)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_never_report_a_change_when_the_interval_is_zero() {
        let dir = scratch("off");
        let path = dir.join("a.md");
        write_at(&path, "one\n", 1);
        let start = Instant::now();
        let mut watcher = Watcher::new(path.clone(), Duration::ZERO);
        assert!(!watcher.enabled());
        write_at(&path, "two\n", 2);
        assert!(!watcher.changed(after(start, 10_000)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_wait_for_the_file_when_it_is_missing_and_report_it_when_it_returns() {
        let dir = scratch("missing");
        let path = dir.join("a.md");
        write_at(&path, "one\n", 1);
        let start = Instant::now();
        let mut watcher = Watcher::new(path.clone(), Duration::from_millis(10));
        std::fs::remove_file(&path).unwrap();
        assert!(
            !watcher.changed(after(start, 100)),
            "a save-by-rename gap is not a change"
        );
        write_at(&path, "two\n", 2);
        assert!(watcher.changed(after(start, 200)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_report_a_change_when_the_file_did_not_exist_at_construction() {
        let dir = scratch("late");
        let path = dir.join("a.md");
        let start = Instant::now();
        let mut watcher = Watcher::new(path.clone(), Duration::from_millis(10));
        assert!(!watcher.changed(after(start, 100)));
        write_at(&path, "hello\n", 1);
        assert!(watcher.changed(after(start, 200)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    // WatchedPage, on the layout and without a display.

    fn numbered(count: usize) -> String {
        (0..count).map(|n| format!("line {n}\n\n")).collect()
    }

    fn paint(page: &mut WatchedPage, width: u32, height: u32) {
        let mut pixels = vec![0; (width * height) as usize];
        page.paint(&mut Canvas::new(&mut pixels, width, height), 1.0);
    }

    fn open(path: &Path, settings: &Settings) -> WatchedPage {
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let page = DocPage::open(path, settings, Scheme::Dark, fonts).unwrap();
        WatchedPage::new(page, path.to_path_buf(), settings)
            .with_locator(|_: &str| None)
            .with_cache_probe(|_, _| true)
    }

    /// Let the worker thread finish and install what it parsed.
    fn settle(page: &mut WatchedPage) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while page.page().parsing() {
            assert!(Instant::now() < deadline, "the parse never finished");
            page.page_mut().tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        page.page_mut().tick();
    }

    fn fast_watch() -> Settings {
        Settings::parse("[view]\nwatch_interval_ms = 1\n").unwrap()
    }

    #[test]
    fn should_sleep_for_the_watch_interval_when_the_page_is_idle() {
        let dir = scratch("idle-period");
        let path = dir.join("a.md");
        write_at(&path, "plain\n", 1);
        let settings = Settings::default();
        let mut watched = open(&path, &settings);
        paint(&mut watched, 400, 200);
        assert!(watched.replan, "a fresh page plans its helpers first");
        watched.step(Instant::now());
        assert!(watched.wants_ticks());
        let period = watched.tick_period();
        assert_eq!(
            period,
            Duration::from_millis(settings.view.watch_interval_ms)
        );
        assert!(
            period > TICK_INTERVAL * 4,
            "an idle viewer must not wake at the editor's rate"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_poll_quickly_when_a_helper_is_still_running() {
        let dir = scratch("busy-period");
        let path = dir.join("a.md");
        write_at(&path, "plain\n", 1);
        let watched = open(&path, &Settings::default());
        assert!(watched.replan);
        assert_eq!(watched.tick_period(), HELPER_POLL_INTERVAL);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_not_tick_at_all_when_watching_is_off_and_nothing_is_pending() {
        let dir = scratch("no-ticks");
        let path = dir.join("a.md");
        write_at(&path, "plain\n", 1);
        let settings = Settings::parse("[view]\nwatch_interval_ms = 0\n").unwrap();
        let mut watched = open(&path, &settings);
        watched.step(Instant::now());
        assert!(!watched.wants_ticks());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn block_at_top(page: &WatchedPage) -> String {
        let doc = page.page().doc();
        let layout = page.page().layout().unwrap();
        let index = layout.block_at(page.page().scroll()).unwrap();
        doc.source[doc.blocks[index].src.clone()].trim().to_owned()
    }

    #[test]
    fn should_reload_and_keep_the_top_block_when_text_is_added_above() {
        let dir = scratch("anchor");
        let path = dir.join("doc.md");
        write_at(&path, &numbered(120), 1);
        let mut page = open(&path, &fast_watch());
        paint(&mut page, 500, 300);
        page.page_mut()
            .handle_key(&winit::keyboard::Key::Character("G".into()), false);
        paint(&mut page, 500, 300);
        let before = block_at_top(&page);
        assert_ne!(before, "line 0", "the page scrolled");
        let mut changed = "A new first paragraph.\n\nAnd another.\n\n".to_owned();
        changed.push_str(&numbered(120));
        write_at(&path, &changed, 2);
        let start = Instant::now();
        assert!(page.step(after(start, 0)));
        settle(&mut page);
        paint(&mut page, 500, 300);
        assert_eq!(page.page().doc().blocks.len(), 122);
        assert_eq!(block_at_top(&page), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_not_reload_when_the_file_only_has_a_new_timestamp() {
        let dir = scratch("touch");
        let path = dir.join("doc.md");
        write_at(&path, &numbered(5), 1);
        let mut page = open(&path, &fast_watch());
        paint(&mut page, 500, 300);
        write_at(&path, &numbered(5), 2);
        assert!(!page.step(Instant::now()), "same bytes, nothing to redraw");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_keep_the_old_text_when_the_file_vanishes() {
        let dir = scratch("vanish");
        let path = dir.join("doc.md");
        write_at(&path, &numbered(5), 1);
        let mut page = open(&path, &fast_watch());
        paint(&mut page, 500, 300);
        std::fs::remove_file(&path).unwrap();
        assert!(!page.step(Instant::now()));
        assert_eq!(page.page().doc().blocks.len(), 5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_survive_invalid_utf8_when_the_file_is_rewritten() {
        let dir = scratch("utf8");
        let path = dir.join("doc.md");
        write_at(&path, "fine\n", 1);
        let mut page = open(&path, &fast_watch());
        paint(&mut page, 500, 300);
        std::fs::write(&path, b"bad \xff\xfe bytes\n").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_009))
            .unwrap();
        assert!(page.step(Instant::now()));
        settle(&mut page);
        paint(&mut page, 500, 300);
        assert!(page.page().doc().source.contains("bad"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_ask_for_ticks_while_watching_and_not_when_watching_is_off() {
        let dir = scratch("ticks");
        let path = dir.join("doc.md");
        write_at(&path, "text\n", 1);
        let mut watching = open(&path, &fast_watch());
        watching.step(Instant::now());
        assert!(watching.wants_ticks());
        let off = Settings::parse("[view]\nwatch_interval_ms = 0\n").unwrap();
        let mut quiet = open(&path, &off);
        quiet.step(Instant::now());
        assert!(!quiet.wants_ticks(), "nothing to poll, no wakeups");
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Helpers through the page.

    const MATH_PAGE: &str = "Some $x^2$ here.\n\n$$y$$\n";

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn drain(page: &mut WatchedPage) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut changed = false;
        while Instant::now() < deadline {
            changed |= page.page_mut().tick();
            changed |= page.step(Instant::now());
            if !page.page().parsing()
                && !page.helpers.busy()
                && page.deferred.is_empty()
                && !page.replan
            {
                return changed;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("helpers never finished");
    }

    #[test]
    fn should_refresh_the_page_when_a_helper_finishes_and_the_cache_has_the_hash() {
        let dir = scratch("helper");
        let path = dir.join("doc.md");
        write_at(&path, MATH_PAGE, 1);
        let ran = dir.join("ran");
        let fake_core = script(
            &dir,
            "limpid-core",
            // Slow enough that the first step sees it running, before the
            // cache is marked ready.
            &format!("sleep 0.3; echo \"$@\" > {}", ran.display()),
        );
        let ready = Rc::new(std::cell::Cell::new(false));
        let probe_ready = ready.clone();
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let settings = Settings::parse("[view]\nwatch_interval_ms = 0\n").unwrap();
        let page = DocPage::open(&path, &settings, Scheme::Dark, fonts).unwrap();
        let locator = fake_core.clone();
        let mut watched = WatchedPage::new(page, path.clone(), &settings)
            .with_locator(move |name: &str| (name == "limpid-core").then(|| locator.clone()))
            .with_cache_probe(move |_, _| probe_ready.get());
        paint(&mut watched, 500, 300);
        watched.step(Instant::now());
        assert!(watched.helpers.busy() || ran.exists());
        ready.set(true);
        let changed = drain(&mut watched);
        assert!(
            changed,
            "the page repaints once the helper's output is cached"
        );
        let args = std::fs::read_to_string(&ran).unwrap();
        assert_eq!(args.trim(), format!("math {}", path.display()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_flash_the_first_error_line_when_a_helper_fails() {
        let dir = scratch("helper-fails");
        let path = dir.join("doc.md");
        write_at(&path, MATH_PAGE, 1);
        let fake_core = script(
            &dir,
            "limpid-core",
            "echo 'pdflatex: Undefined control sequence' >&2; echo more >&2; exit 1",
        );
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let settings = Settings::parse("[view]\nwatch_interval_ms = 0\n").unwrap();
        let page = DocPage::open(&path, &settings, Scheme::Dark, fonts).unwrap();
        let locator = fake_core.clone();
        let mut watched = WatchedPage::new(page, path.clone(), &settings)
            .with_locator(move |name: &str| (name == "limpid-core").then(|| locator.clone()))
            .with_cache_probe(|_, _| false);
        paint(&mut watched, 500, 300);
        let changed = drain(&mut watched);
        assert!(changed, "the flash must repaint the page");
        let now = watched.page().elapsed_ms();
        let message = watched.page().nav().status_view(now).message;
        assert_eq!(message, "math: pdflatex: Undefined control sequence");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_run_a_helper_only_once_when_it_leaves_the_cache_empty() {
        let dir = scratch("once-helper");
        let path = dir.join("doc.md");
        write_at(&path, MATH_PAGE, 1);
        let count = dir.join("count");
        let fake_core = script(
            &dir,
            "limpid-core",
            &format!("echo x >> {}", count.display()),
        );
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let settings = Settings::parse("[view]\nwatch_interval_ms = 1\n").unwrap();
        let page = DocPage::open(&path, &settings, Scheme::Dark, fonts).unwrap();
        let locator = fake_core.clone();
        let mut watched = WatchedPage::new(page, path.clone(), &settings)
            .with_locator(move |name: &str| (name == "limpid-core").then(|| locator.clone()))
            .with_cache_probe(|_, _| false);
        drain(&mut watched);
        write_at(&path, &format!("{MATH_PAGE}\nmore text\n"), 2);
        std::thread::sleep(Duration::from_millis(5));
        watched.step(Instant::now());
        drain(&mut watched);
        let runs = std::fs::read_to_string(&count).unwrap().lines().count();
        assert_eq!(
            runs, 1,
            "the failed formulas are not retried on every reload"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_run_a_helper_for_new_formulas_when_a_reload_adds_them() {
        let dir = scratch("new-formula");
        let path = dir.join("doc.md");
        write_at(&path, MATH_PAGE, 1);
        let count = dir.join("count");
        let fake_core = script(
            &dir,
            "limpid-core",
            &format!("echo x >> {}", count.display()),
        );
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let settings = Settings::parse("[view]\nwatch_interval_ms = 1\n").unwrap();
        let page = DocPage::open(&path, &settings, Scheme::Dark, fonts).unwrap();
        let locator = fake_core.clone();
        let mut watched = WatchedPage::new(page, path.clone(), &settings)
            .with_locator(move |name: &str| (name == "limpid-core").then(|| locator.clone()))
            .with_cache_probe(|_, _| false);
        drain(&mut watched);
        write_at(&path, &format!("{MATH_PAGE}\n$$z^3$$\n"), 2);
        std::thread::sleep(Duration::from_millis(5));
        watched.step(Instant::now());
        drain(&mut watched);
        let runs = std::fs::read_to_string(&count).unwrap().lines().count();
        assert_eq!(runs, 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Reloads read once and parse once, off the UI thread.

    fn open_slow(path: &Path, settings: &Settings, delay: Duration) -> WatchedPage {
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let loader = crate::page::load::Loader::new(Mode::File, settings).with_parser(
            std::sync::Arc::new(move |source, mode, settings| {
                std::thread::sleep(delay);
                limpid_core::doc::parse(source, mode, settings)
            }),
        );
        let page = DocPage::open_async_with(path, settings, Scheme::Dark, fonts, loader).unwrap();
        let mut watched = WatchedPage::new(page, path.to_path_buf(), settings)
            .with_locator(|_: &str| None)
            .with_cache_probe(|_, _| true);
        settle(&mut watched);
        watched
    }

    #[test]
    fn should_parse_once_when_the_file_changes() {
        let dir = scratch("parse-once");
        let path = dir.join("doc.md");
        write_at(&path, "one\n", 1);
        let mut page = open(&path, &fast_watch());
        let before = page.page().parse_count();
        write_at(&path, "two\n", 2);
        std::thread::sleep(Duration::from_millis(5));
        assert!(page.step(Instant::now()));
        settle(&mut page);
        assert_eq!(page.page().parse_count(), before + 1);
        assert!(page.page().doc().source.contains("two"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_not_parse_again_when_the_same_text_is_saved_while_its_parse_runs() {
        let dir = scratch("same-twice");
        let path = dir.join("doc.md");
        write_at(&path, "one\n", 1);
        let mut page = open_slow(&path, &fast_watch(), Duration::from_millis(150));
        let before = page.page().parse_count();
        write_at(&path, "two\n", 2);
        std::thread::sleep(Duration::from_millis(5));
        assert!(page.step(Instant::now()));
        assert!(page.page().parsing());
        write_at(&path, "two\n", 3);
        std::thread::sleep(Duration::from_millis(5));
        assert!(
            !page.step(Instant::now()),
            "the text on its way is the same"
        );
        settle(&mut page);
        assert_eq!(page.page().parse_count(), before + 1);
        assert!(page.page().doc().source.contains("two"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_show_the_last_text_when_the_file_changes_back_during_a_parse() {
        let dir = scratch("back-again");
        let path = dir.join("doc.md");
        write_at(&path, "one\n", 1);
        let mut page = open_slow(&path, &fast_watch(), Duration::from_millis(150));
        write_at(&path, "two\n", 2);
        std::thread::sleep(Duration::from_millis(5));
        assert!(page.step(Instant::now()));
        // Back to what the page still shows, before "two" has landed.
        write_at(&path, "one\n", 3);
        std::thread::sleep(Duration::from_millis(5));
        assert!(page.step(Instant::now()), "one is not what is on its way");
        settle(&mut page);
        assert!(
            page.page().doc().source.contains("one"),
            "the stale parse lost"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_wait_for_the_parse_before_planning_helpers_when_the_file_changes() {
        let dir = scratch("plan-after-parse");
        let path = dir.join("doc.md");
        write_at(&path, "plain\n", 1);
        let mut page = open_slow(&path, &fast_watch(), Duration::from_millis(150));
        page.step(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
        write_at(&path, "plain\n\nnow $x$ here\n", 2);
        std::thread::sleep(Duration::from_millis(5));
        page.step(Instant::now());
        assert!(page.replan, "planning waits for the new text");
        settle(&mut page);
        page.step(Instant::now());
        assert!(!page.replan, "planned from the new text");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_follow_the_page_mode_when_the_page_is_an_agent_page() {
        let dir = scratch("agent-mode");
        let path = dir.join("doc.md");
        write_at(&path, "text\n", 1);
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let settings = fast_watch();
        let page = DocPage::open_as(&path, &settings, Scheme::Dark, fonts, Mode::Agent).unwrap();
        let mut watched = WatchedPage::new(page, path.clone(), &settings);
        assert_eq!(watched.mode, Mode::Agent);
        write_at(&path, "<script>x</script>\n\ntext\n", 2);
        std::thread::sleep(Duration::from_millis(5));
        assert!(watched.step(Instant::now()));
        settle(&mut watched);
        assert_eq!(watched.page().mode(), Mode::Agent);
        assert!(
            format!("{:?}", watched.page().doc().blocks).contains("<script>x</script>"),
            "a reload parses in the page's mode"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
