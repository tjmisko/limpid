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
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// A math run that takes longer than this is killed. Typesetting a page is
/// seconds of work; this only bounds a wedged TeX.
pub const MATH_TIME_LIMIT: Duration = Duration::from_secs(120);

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

/// The Mermaid fences of `source`, as `(hash)`, in document order.
pub fn mermaid_hashes(source: &str, settings: &Settings) -> Vec<String> {
    let doc = parse(source, Mode::File, settings);
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
                time_limit: MATH_TIME_LIMIT,
            });
        }
    }

    if settings.mermaid.enabled {
        let hashes: Vec<String> = mermaid_hashes(source, settings)
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
                time_limit: Duration::from_secs_f64(settings.mermaid.timeout_seconds)
                    + MERMAID_GRACE,
            });
        }
    }
    jobs
}

struct Running {
    job: HelperJob,
    child: Child,
    started: Instant,
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
                Ok(child) => self.running.push(Running {
                    job,
                    child,
                    started: Instant::now(),
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
            let exited = matches!(run.child.try_wait(), Ok(Some(_)) | Err(_));
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
            ready.extend(run.job.hashes.into_iter().filter(|hash| cached(kind, hash)));
        }
        ready
    }
}

impl Drop for Helpers {
    fn drop(&mut self) {
        for run in &mut self.running {
            run.kill();
        }
    }
}

fn spawn(job: &HelperJob) -> std::io::Result<Child> {
    Command::new(&job.program)
        .args(&job.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
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
        let hashes = mermaid_hashes(source, &Settings::default());
        assert_eq!(hashes.len(), 2, "{hashes:?}");
    }

    #[test]
    fn should_list_a_repeated_diagram_once_when_collecting() {
        let fence = "```mermaid\ngraph LR; A-->B\n```\n\n";
        let hashes = mermaid_hashes(&format!("{fence}{fence}"), &Settings::default());
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
}
