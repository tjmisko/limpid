//! Helper processes that fill the math and Mermaid caches for an open page.
//!
//! The page shows source until the cache has an SVG for a block. After a page
//! opens (and after each reload) the viewer plans a job for each kind of
//! block that is not cached yet and runs it as a child process: the viewer
//! never typesets or renders diagrams itself, and never waits for a child.
//! [`Helpers::poll`] is called from the page's tick and reports which hashes
//! reached the cache so the layout can draw them.

use crate::layout::boxes::mermaid_root;
use limpid_core::{
    doc::{Block, BlockKind, Mode, parse},
    math,
    settings::{MathBackend, Settings},
};
use std::{
    collections::HashSet,
    ffi::OsString,
    io::Read,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{Receiver, channel},
    thread,
    time::{Duration, Instant},
};

/// How much of a helper's stderr is kept to find its first error line.
const STDERR_CAPTURE_BYTES: u64 = 16 * 1024;

/// How long to wait for the stderr reader after a child has exited. A
/// grandchild that kept the pipe open must not stall the page.
const STDERR_DRAIN_WAIT: Duration = Duration::from_millis(200);

/// Longest status line a helper failure puts on the page.
const FLASH_LIMIT_CHARS: usize = 120;

/// Added to `mermaid.timeout_seconds` before the viewer gives up on the host
/// helper, which enforces the timeout itself.
pub const MERMAID_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HelperKind {
    Math,
    Mermaid,
}

/// One child process to run, and the hashes it is expected to produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperJob {
    pub kind: HelperKind,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub hashes: Vec<String>,
    pub time_limit: Duration,
}

/// Where to find a helper program by name.
pub trait Locate {
    fn locate(&self, name: &str) -> Option<PathBuf>;
}

impl<F: Fn(&str) -> Option<PathBuf>> Locate for F {
    fn locate(&self, name: &str) -> Option<PathBuf> {
        self(name)
    }
}

/// Next to the running executable first (the install puts them together),
/// then on `PATH`.
pub fn locate_installed(name: &str) -> Option<PathBuf> {
    let sibling = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(name))
        .filter(|candidate| candidate.is_file());
    sibling.or_else(|| math::find_binary(name, None))
}

/// A duration of `seconds` that never panics: a negative or NaN value is
/// zero, and one too large to represent saturates at [`Duration::MAX`].
pub fn duration_from_seconds(seconds: f64) -> Duration {
    if seconds.is_nan() || seconds <= 0.0 {
        return Duration::ZERO;
    }
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX)
}

/// The Mermaid fences of `source`, as `(hash)`, in document order.
pub fn mermaid_hashes(source: &str, mode: Mode, settings: &Settings) -> Vec<String> {
    let doc = parse(source, mode, settings);
    let mut seen = HashSet::new();
    let mut hashes = Vec::new();
    collect_mermaid(&doc.blocks, &mut seen, &mut hashes);
    hashes
}

fn collect_mermaid(blocks: &[Block], seen: &mut HashSet<String>, hashes: &mut Vec<String>) {
    for block in blocks {
        match &block.kind {
            BlockKind::Mermaid { hash, .. } => {
                if seen.insert(hash.clone()) {
                    hashes.push(hash.clone());
                }
            }
            BlockKind::List { items, .. } => {
                for item in items {
                    collect_mermaid(&item.blocks, seen, hashes);
                }
            }
            BlockKind::Quote { blocks, .. } | BlockKind::FootnoteDef { blocks, .. } => {
                collect_mermaid(blocks, seen, hashes);
            }
            _ => {}
        }
    }
}

/// Whether the cache already holds the output for `hash`.
pub fn is_cached(kind: HelperKind, hash: &str) -> bool {
    match kind {
        HelperKind::Math => math::cached(hash).is_some(),
        HelperKind::Mermaid => {
            mermaid_root().is_some_and(|root| root.join(format!("{hash}.svg")).is_file())
        }
    }
}

/// The jobs an open page needs: one per kind with hashes that are not cached
/// and not in `attempted`. A kind is skipped when its setting turns it off or
/// its program is not installed, so the page keeps showing source.
pub fn plan(
    source: &str,
    mode: Mode,
    file: &Path,
    settings: &Settings,
    attempted: &HashSet<String>,
    locate: &dyn Locate,
    cached: &dyn Fn(HelperKind, &str) -> bool,
) -> Vec<HelperJob> {
    let mut jobs = Vec::new();
    let wanted = |kind, hash: &String| !attempted.contains(hash) && !cached(kind, hash);

    if settings.math.backend != MathBackend::None {
        let hashes: Vec<String> = math::collect(source)
            .into_iter()
            .map(|formula| formula.hash)
            .filter(|hash| wanted(HelperKind::Math, hash))
            .collect();
        if !hashes.is_empty()
            && let Some(program) = locate.locate("limpid-core")
        {
            jobs.push(HelperJob {
                kind: HelperKind::Math,
                program,
                args: vec!["math".into(), file.into()],
                hashes,
                time_limit: duration_from_seconds(settings.math.timeout_seconds),
            });
        }
    }

    if settings.mermaid.enabled {
        let hashes: Vec<String> = mermaid_hashes(source, mode, settings)
            .into_iter()
            .filter(|hash| wanted(HelperKind::Mermaid, hash))
            .collect();
        if !hashes.is_empty()
            && let Some(program) = locate.locate("limpid-host")
        {
            jobs.push(HelperJob {
                kind: HelperKind::Mermaid,
                program,
                args: vec!["--render-mermaid".into(), file.into()],
                hashes,
                time_limit: duration_from_seconds(settings.mermaid.timeout_seconds)
                    .saturating_add(MERMAID_GRACE),
            });
        }
    }
    jobs
}

struct Running {
    job: HelperJob,
    child: Child,
    started: Instant,
    stderr: Receiver<String>,
}

/// A helper that exited non-zero or ran out of time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperFailure {
    pub kind: HelperKind,
    /// The first error line the helper printed, or a description of how it
    /// ended when it printed none.
    pub message: String,
}

impl HelperFailure {
    /// The text for the page status line.
    pub fn flash_text(&self) -> String {
        let name = match self.kind {
            HelperKind::Math => "math",
            HelperKind::Mermaid => "mermaid",
        };
        let text = format!("{name}: {}", self.message);
        if text.chars().count() <= FLASH_LIMIT_CHARS {
            return text;
        }
        let clipped: String = text.chars().take(FLASH_LIMIT_CHARS - 1).collect();
        format!("{clipped}\u{2026}")
    }
}

/// The first line of `stderr` that says anything.
pub fn first_error_line(stderr: &str) -> Option<&str> {
    stderr.lines().map(str::trim).find(|line| !line.is_empty())
}

impl Running {
    /// Kill the child and everything it started (WebKit spawns its own).
    fn kill(&mut self) {
        // SAFETY: the child leads its own process group (see `spawn`), so the
        // group id is the child's pid; killing a gone group is harmless.
        unsafe {
            libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The children running for one page.
#[derive(Default)]
pub struct Helpers {
    running: Vec<Running>,
    failures: Vec<HelperFailure>,
}

impl Helpers {
    /// Start `jobs`, except those whose kind already has a child running.
    /// Returns the jobs that were not started, for a later try.
    pub fn launch(&mut self, jobs: Vec<HelperJob>) -> Vec<HelperJob> {
        let mut deferred = Vec::new();
        for job in jobs {
            if self.running.iter().any(|run| run.job.kind == job.kind) {
                deferred.push(job);
                continue;
            }
            match spawn(&job) {
                Ok((child, stderr)) => self.running.push(Running {
                    job,
                    child,
                    started: Instant::now(),
                    stderr,
                }),
                Err(error) => eprintln!("limpid-view: {}: {error}", job.program.display()),
            }
        }
        deferred
    }

    pub fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// Reap children that exited or ran out of time. Returns the hashes of
    /// those children that are now cached; the caller refreshes the layout.
    pub fn poll(&mut self, now: Instant, cached: &dyn Fn(HelperKind, &str) -> bool) -> Vec<String> {
        let mut ready = Vec::new();
        let mut index = 0;
        while index < self.running.len() {
            let run = &mut self.running[index];
            let status = run.child.try_wait();
            let exited = matches!(status, Ok(Some(_)) | Err(_));
            let expired = now.saturating_duration_since(run.started) > run.job.time_limit;
            if !exited && !expired {
                index += 1;
                continue;
            }
            let mut run = self.running.swap_remove(index);
            if !exited {
                run.kill();
            }
            let kind = run.job.kind;
            let failed = !exited || matches!(status, Ok(Some(status)) if !status.success());
            if failed {
                let message = failure_message(&run, exited);
                eprintln!("limpid-view: {kind:?} helper failed: {message}");
                self.failures.push(HelperFailure { kind, message });
            }
            ready.extend(run.job.hashes.into_iter().filter(|hash| cached(kind, hash)));
        }
        ready
    }
}

impl Helpers {
    /// The failures seen since the last call, for the page status.
    pub fn take_failures(&mut self) -> Vec<HelperFailure> {
        std::mem::take(&mut self.failures)
    }
}

fn failure_message(run: &Running, exited: bool) -> String {
    let stderr = run
        .stderr
        .recv_timeout(STDERR_DRAIN_WAIT)
        .unwrap_or_default();
    if let Some(line) = first_error_line(&stderr) {
        return line.to_owned();
    }
    if !exited {
        return "timed out".into();
    }
    "failed without output".into()
}

impl Drop for Helpers {
    fn drop(&mut self) {
        for run in &mut self.running {
            run.kill();
        }
    }
}

fn spawn(job: &HelperJob) -> std::io::Result<(Child, Receiver<String>)> {
    let mut child = Command::new(&job.program)
        .args(&job.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let (sender, receiver) = channel();
    if let Some(mut pipe) = child.stderr.take() {
        thread::spawn(move || {
            let mut kept = Vec::new();
            let _ = (&mut pipe)
                .take(STDERR_CAPTURE_BYTES)
                .read_to_end(&mut kept);
            let _ = sender.send(String::from_utf8_lossy(&kept).into_owned());
            // Keep draining so a chatty child never blocks on a full pipe.
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        });
    }
    Ok((child, receiver))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(source: &str) -> Settings {
        Settings::parse(source).unwrap()
    }

    fn everything_installed(name: &str) -> Option<PathBuf> {
        Some(PathBuf::from(format!("/opt/bin/{name}")))
    }

    fn nothing_cached(_: HelperKind, _: &str) -> bool {
        false
    }

    const PAGE: &str =
        "Inline $x^2$ and\n\n$$\\int_0^1 x\\,dx$$\n\n```mermaid\ngraph TD; A-->B\n```\n";

    fn plan_for(
        source: &str,
        settings: &Settings,
        attempted: &HashSet<String>,
        locate: &dyn Locate,
        cached: &dyn Fn(HelperKind, &str) -> bool,
    ) -> Vec<HelperJob> {
        plan(
            source,
            Mode::File,
            Path::new("/n/a.md"),
            settings,
            attempted,
            locate,
            cached,
        )
    }

    #[test]
    fn should_plan_a_math_and_a_mermaid_job_when_both_are_uncached() {
        let jobs = plan_for(
            PAGE,
            &Settings::default(),
            &HashSet::new(),
            &everything_installed,
            &nothing_cached,
        );
        let kinds: Vec<_> = jobs.iter().map(|job| job.kind).collect();
        assert_eq!(kinds, [HelperKind::Math, HelperKind::Mermaid]);
        assert_eq!(jobs[0].hashes.len(), 2);
        assert_eq!(jobs[1].hashes.len(), 1);
        assert_eq!(
            jobs[0].args,
            [OsString::from("math"), OsString::from("/n/a.md")]
        );
        assert_eq!(
            jobs[1].args,
            [
                OsString::from("--render-mermaid"),
                OsString::from("/n/a.md")
            ]
        );
        assert_eq!(jobs[0].program, PathBuf::from("/opt/bin/limpid-core"));
        assert_eq!(jobs[1].program, PathBuf::from("/opt/bin/limpid-host"));
    }

    #[test]
    fn should_plan_nothing_when_everything_is_cached() {
        let jobs = plan_for(
            PAGE,
            &Settings::default(),
            &HashSet::new(),
            &everything_installed,
            &|_, _| true,
        );
        assert!(jobs.is_empty());
    }

    #[test]
    fn should_plan_only_the_missing_hashes_when_some_are_cached() {
        let all = plan_for(
            PAGE,
            &Settings::default(),
            &HashSet::new(),
            &everything_installed,
            &nothing_cached,
        );
        let cached_hash = all[0].hashes[0].clone();
        let jobs = plan_for(
            PAGE,
            &Settings::default(),
            &HashSet::new(),
            &everything_installed,
            &|kind, hash| kind == HelperKind::Math && hash == cached_hash,
        );
        assert_eq!(jobs[0].hashes.len(), 1);
        assert!(!jobs[0].hashes.contains(&cached_hash));
    }

    #[test]
    fn should_skip_hashes_already_attempted_when_a_helper_failed_before() {
        let all = plan_for(
            PAGE,
            &Settings::default(),
            &HashSet::new(),
            &everything_installed,
            &nothing_cached,
        );
        let attempted: HashSet<String> = all
            .iter()
            .flat_map(|job| job.hashes.iter().cloned())
            .collect();
        let jobs = plan_for(
            PAGE,
            &Settings::default(),
            &attempted,
            &everything_installed,
            &nothing_cached,
        );
        assert!(jobs.is_empty(), "a failing helper must not respawn forever");
    }

    #[test]
    fn should_skip_mermaid_when_the_setting_is_off() {
        let jobs = plan_for(
            PAGE,
            &settings("[mermaid]\nenabled = false\n"),
            &HashSet::new(),
            &everything_installed,
            &nothing_cached,
        );
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, HelperKind::Math);
    }

    #[test]
    fn should_skip_math_when_the_backend_is_none() {
        let jobs = plan_for(
            PAGE,
            &settings("[math]\nbackend = 'none'\n"),
            &HashSet::new(),
            &everything_installed,
            &nothing_cached,
        );
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, HelperKind::Mermaid);
    }

    #[test]
    fn should_skip_a_kind_when_its_program_is_not_installed() {
        let only_core = |name: &str| (name == "limpid-core").then(|| PathBuf::from(name));
        let jobs = plan_for(
            PAGE,
            &Settings::default(),
            &HashSet::new(),
            &only_core,
            &nothing_cached,
        );
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, HelperKind::Math);
    }

    #[test]
    fn should_not_count_math_or_diagrams_inside_code_when_planning() {
        let source = "`$x$` and\n\n```\n$$y$$\n```\n\n```rust\nlet a = \"$z$\";\n```\n";
        let jobs = plan_for(
            source,
            &Settings::default(),
            &HashSet::new(),
            &everything_installed,
            &nothing_cached,
        );
        assert!(jobs.is_empty(), "{jobs:?}");
    }

    #[test]
    fn should_find_diagrams_inside_lists_and_quotes_when_collecting() {
        let source = "- item\n\n  ```mermaid\n  graph LR; A-->B\n  ```\n\n> ```mermaid\n> graph LR; C-->D\n> ```\n";
        let hashes = mermaid_hashes(source, Mode::File, &Settings::default());
        assert_eq!(hashes.len(), 2, "{hashes:?}");
    }

    #[test]
    fn should_list_a_repeated_diagram_once_when_collecting() {
        let fence = "```mermaid\ngraph LR; A-->B\n```\n\n";
        let hashes = mermaid_hashes(&format!("{fence}{fence}"), Mode::File, &Settings::default());
        assert_eq!(hashes.len(), 1);
    }

    #[test]
    fn should_give_mermaid_the_configured_timeout_plus_grace_when_planning() {
        let jobs = plan_for(
            PAGE,
            &settings("[mermaid]\ntimeout_seconds = 5\n"),
            &HashSet::new(),
            &everything_installed,
            &nothing_cached,
        );
        assert_eq!(jobs[1].time_limit, Duration::from_secs(5) + MERMAID_GRACE);
    }

    fn shell_job(kind: HelperKind, script: &str, hashes: &[&str], limit: Duration) -> HelperJob {
        HelperJob {
            kind,
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), script.into()],
            hashes: hashes.iter().map(|hash| (*hash).to_owned()).collect(),
            time_limit: limit,
        }
    }

    fn wait_for(helpers: &mut Helpers, cached: &dyn Fn(HelperKind, &str) -> bool) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let ready = helpers.poll(Instant::now(), cached);
            if !helpers.busy() {
                return ready;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("helper never finished");
    }

    #[test]
    fn should_report_cached_hashes_when_a_child_exits() {
        let mut helpers = Helpers::default();
        let deferred = helpers.launch(vec![shell_job(
            HelperKind::Math,
            "exit 0",
            &["aa", "bb"],
            Duration::from_secs(30),
        )]);
        assert!(deferred.is_empty());
        let ready = wait_for(&mut helpers, &|_, hash| hash == "aa");
        assert_eq!(ready, ["aa"], "only what reached the cache is refreshed");
    }

    #[test]
    fn should_report_cached_hashes_when_a_child_fails() {
        let mut helpers = Helpers::default();
        helpers.launch(vec![shell_job(
            HelperKind::Mermaid,
            "exit 3",
            &["aa"],
            Duration::from_secs(30),
        )]);
        let ready = wait_for(&mut helpers, &|_, _| true);
        assert_eq!(ready, ["aa"]);
    }

    #[test]
    fn should_not_block_the_caller_when_a_child_runs_long() {
        let mut helpers = Helpers::default();
        let started = Instant::now();
        helpers.launch(vec![shell_job(
            HelperKind::Math,
            "sleep 30",
            &["aa"],
            Duration::from_secs(60),
        )]);
        assert!(helpers.poll(Instant::now(), &|_, _| true).is_empty());
        assert!(helpers.busy());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn should_kill_a_child_and_its_children_when_the_time_limit_passes() {
        let marker =
            std::env::temp_dir().join(format!("limpid-helper-kill-{}", std::process::id()));
        let script = format!("sleep 30 & echo $! > {}; wait", marker.display());
        let mut helpers = Helpers::default();
        helpers.launch(vec![shell_job(
            HelperKind::Math,
            &script,
            &["aa"],
            Duration::from_millis(1),
        )]);
        let grandchild = loop {
            if let Ok(text) = std::fs::read_to_string(&marker)
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                break pid;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        std::thread::sleep(Duration::from_millis(20));
        let ready = helpers.poll(Instant::now(), &|_, _| false);
        assert!(ready.is_empty());
        assert!(!helpers.busy());
        std::thread::sleep(Duration::from_millis(100));
        let alive = std::path::Path::new(&format!("/proc/{grandchild}")).exists()
            && std::fs::read_to_string(format!("/proc/{grandchild}/stat"))
                .is_ok_and(|stat| !stat.contains(") Z"));
        assert!(!alive, "the grandchild must not outlive the helper");
        std::fs::remove_file(marker).unwrap();
    }

    #[test]
    fn should_kill_running_children_when_dropped() {
        let marker =
            std::env::temp_dir().join(format!("limpid-helper-drop-{}", std::process::id()));
        let script = format!("echo $$ > {}; sleep 30", marker.display());
        let mut helpers = Helpers::default();
        helpers.launch(vec![shell_job(
            HelperKind::Math,
            &script,
            &["aa"],
            Duration::from_secs(60),
        )]);
        let pid = loop {
            if let Ok(text) = std::fs::read_to_string(&marker)
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                break pid;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        drop(helpers);
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "dropping the page must not leave a helper behind"
        );
        std::fs::remove_file(marker).unwrap();
    }

    #[test]
    fn should_defer_a_second_job_of_the_same_kind_when_one_is_running() {
        let mut helpers = Helpers::default();
        helpers.launch(vec![shell_job(
            HelperKind::Math,
            "sleep 30",
            &["aa"],
            Duration::from_secs(60),
        )]);
        let deferred = helpers.launch(vec![
            shell_job(HelperKind::Math, "exit 0", &["bb"], Duration::from_secs(60)),
            shell_job(
                HelperKind::Mermaid,
                "sleep 30",
                &["cc"],
                Duration::from_secs(60),
            ),
        ]);
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].hashes, ["bb"]);
    }

    #[test]
    fn should_survive_a_missing_program_when_launching() {
        let mut helpers = Helpers::default();
        let deferred = helpers.launch(vec![HelperJob {
            kind: HelperKind::Math,
            program: PathBuf::from("/nonexistent/limpid-core"),
            args: vec![],
            hashes: vec!["aa".into()],
            time_limit: Duration::from_secs(1),
        }]);
        assert!(deferred.is_empty());
        assert!(!helpers.busy());
    }

    #[test]
    fn should_not_panic_when_building_durations_at_the_extremes() {
        assert_eq!(duration_from_seconds(f64::MAX), Duration::MAX);
        assert_eq!(duration_from_seconds(f64::INFINITY), Duration::MAX);
        assert_eq!(duration_from_seconds(f64::NAN), Duration::ZERO);
        assert_eq!(duration_from_seconds(-5.0), Duration::ZERO);
        assert_eq!(duration_from_seconds(0.0), Duration::ZERO);
        assert_eq!(duration_from_seconds(1.5), Duration::from_millis(1500));
    }

    #[test]
    fn should_saturate_the_mermaid_limit_when_the_timeout_is_enormous() {
        let mut huge = Settings::default();
        huge.mermaid.timeout_seconds = f64::MAX;
        let jobs = plan_for(
            PAGE,
            &huge,
            &HashSet::new(),
            &everything_installed,
            &nothing_cached,
        );
        assert_eq!(jobs[1].time_limit, Duration::MAX);
    }

    #[test]
    fn should_use_the_math_timeout_setting_when_planning() {
        let jobs = plan_for(
            PAGE,
            &settings("[math]\ntimeout_seconds = 7\n"),
            &HashSet::new(),
            &everything_installed,
            &nothing_cached,
        );
        assert_eq!(jobs[0].time_limit, Duration::from_secs(7));
    }

    #[test]
    fn should_find_diagrams_in_agent_mode_when_planning() {
        let jobs = plan(
            PAGE,
            Mode::Agent,
            Path::new("/n/a.md"),
            &Settings::default(),
            &HashSet::new(),
            &everything_installed,
            &nothing_cached,
        );
        assert_eq!(jobs.len(), 2);
    }

    fn wait_for_failures(helpers: &mut Helpers) -> Vec<HelperFailure> {
        wait_for(helpers, &|_, _| false);
        helpers.take_failures()
    }

    #[test]
    fn should_report_the_first_error_line_when_a_helper_exits_nonzero() {
        let mut helpers = Helpers::default();
        helpers.launch(vec![shell_job(
            HelperKind::Math,
            "printf '\\n  \\nlatex: bad formula\\nsecond line\\n' >&2; exit 1",
            &["aa"],
            Duration::from_secs(30),
        )]);
        let failures = wait_for_failures(&mut helpers);
        assert_eq!(
            failures,
            [HelperFailure {
                kind: HelperKind::Math,
                message: "latex: bad formula".into()
            }]
        );
        assert_eq!(failures[0].flash_text(), "math: latex: bad formula");
        assert!(helpers.take_failures().is_empty(), "failures are drained");
    }

    #[test]
    fn should_say_it_failed_without_output_when_a_helper_exits_nonzero_silently() {
        let mut helpers = Helpers::default();
        helpers.launch(vec![shell_job(
            HelperKind::Mermaid,
            "exit 2",
            &["aa"],
            Duration::from_secs(30),
        )]);
        let failures = wait_for_failures(&mut helpers);
        assert_eq!(failures[0].message, "failed without output");
    }

    #[test]
    fn should_report_no_failure_when_a_helper_exits_zero_with_stderr_noise() {
        let mut helpers = Helpers::default();
        helpers.launch(vec![shell_job(
            HelperKind::Math,
            "echo warning >&2; exit 0",
            &["aa"],
            Duration::from_secs(30),
        )]);
        assert!(wait_for_failures(&mut helpers).is_empty());
    }

    #[test]
    fn should_report_a_timeout_when_a_helper_is_killed_for_running_long() {
        let mut helpers = Helpers::default();
        helpers.launch(vec![shell_job(
            HelperKind::Math,
            "sleep 30",
            &["aa"],
            Duration::from_millis(1),
        )]);
        std::thread::sleep(Duration::from_millis(20));
        let failures = wait_for_failures(&mut helpers);
        assert_eq!(failures[0].message, "timed out");
    }

    #[test]
    fn should_not_block_when_a_helper_floods_stderr() {
        let mut helpers = Helpers::default();
        helpers.launch(vec![shell_job(
            HelperKind::Math,
            "yes 'noise noise noise' | head -c 400000 >&2; exit 1",
            &["aa"],
            Duration::from_secs(30),
        )]);
        let failures = wait_for_failures(&mut helpers);
        assert_eq!(failures[0].message, "noise noise noise");
    }

    #[test]
    fn should_clip_a_long_message_when_building_the_flash() {
        let failure = HelperFailure {
            kind: HelperKind::Mermaid,
            message: "x".repeat(500),
        };
        assert_eq!(failure.flash_text().chars().count(), FLASH_LIMIT_CHARS);
    }
}
