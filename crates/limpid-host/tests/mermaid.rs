//! Behavioral tests for `limpid-host --render-mermaid`. They run the real
//! helper (WebKit, the vendored Mermaid bundle) and are skipped when no
//! display is available. A private `XDG_CACHE_HOME` keeps the user's cache
//! untouched, and `LIMPID_WINDOW_CLASS=limpid-test` keeps any fallback window
//! under the compositor rule for tests.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/two-diagrams.md"
);

fn display_available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

fn private_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("limpid-mermaid-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_helper(markdown: &Path, root: &Path, marker: &str, extra_config: &str) -> Output {
    let config = root.join("config");
    fs::create_dir_all(config.join("limpid")).unwrap();
    fs::write(config.join("limpid/config.toml"), extra_config).unwrap();
    Command::new(env!("CARGO_BIN_EXE_limpid-host"))
        .arg("--render-mermaid")
        .arg(markdown)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_CONFIG_HOME", &config)
        .env("LIMPID_WINDOW_CLASS", "limpid-test")
        .env("LIMPID_TEST_MARKER", marker)
        .output()
        .unwrap()
}

fn cached_svgs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root.join("cache/limpid/mermaid")) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.map(|entry| entry.unwrap().path()).collect();
    paths.sort();
    paths
}

/// Processes whose environment carries `marker` (children inherit it).
fn processes_with_marker(marker: &str) -> Vec<u32> {
    let needle = format!("LIMPID_TEST_MARKER={marker}");
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(environment) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        if environment
            .split(|byte| *byte == 0)
            .any(|entry| entry == needle.as_bytes())
        {
            found.push(pid);
        }
    }
    found
}

fn wait_for_no_processes(marker: &str) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = processes_with_marker(marker);
        if remaining.is_empty() || Instant::now() > deadline {
            return remaining;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn should_render_two_diagrams_then_report_cached_when_run_twice() {
    if !display_available() {
        eprintln!("skipped: no display");
        return;
    }
    let root = private_dir("render");
    let marker = format!("render-{}", std::process::id());

    let first = run_helper(Path::new(FIXTURE), &root, &marker, "");
    let stdout = String::from_utf8_lossy(&first.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(first.status.success(), "stdout {stdout} stderr {stderr}");
    assert_eq!(stdout, "rendered 2 cached 0 failed 0", "stderr {stderr}");

    let svgs = cached_svgs(&root);
    assert_eq!(svgs.len(), 2, "{svgs:?}");
    for path in &svgs {
        let svg = fs::read_to_string(path).unwrap();
        assert!(svg.contains("<svg"), "{path:?}");
        assert!(!svg.contains("foreignObject"), "{path:?}");
        let tree = usvg::Tree::from_str(&svg, &usvg::Options::default())
            .unwrap_or_else(|error| panic!("{path:?} does not parse: {error}"));
        assert!(tree.size().width() > 1.0 && tree.size().height() > 1.0);
    }

    let second = run_helper(Path::new(FIXTURE), &root, &marker, "");
    assert!(second.status.success());
    assert_eq!(
        String::from_utf8_lossy(&second.stdout).trim(),
        "rendered 0 cached 2 failed 0"
    );
    assert_eq!(wait_for_no_processes(&marker), Vec::<u32>::new());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn should_report_failure_and_exit_nonzero_when_the_only_diagram_is_invalid() {
    if !display_available() {
        eprintln!("skipped: no display");
        return;
    }
    let root = private_dir("invalid");
    let marker = format!("invalid-{}", std::process::id());
    let markdown = root.join("broken.md");
    fs::write(&markdown, "```mermaid\nthis is not a diagram ((\n```\n").unwrap();

    let output = run_helper(&markdown, &root, &marker, "");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "rendered 0 cached 0 failed 1"
    );
    assert!(!output.status.success());
    assert!(cached_svgs(&root).is_empty());
    assert_eq!(wait_for_no_processes(&marker), Vec::<u32>::new());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn should_exit_zero_when_one_of_two_diagrams_is_invalid() {
    if !display_available() {
        eprintln!("skipped: no display");
        return;
    }
    let root = private_dir("mixed");
    let marker = format!("mixed-{}", std::process::id());
    let markdown = root.join("mixed.md");
    fs::write(
        &markdown,
        "```mermaid\ngraph TD; A-->B\n```\n\n```mermaid\nnot a diagram ((\n```\n",
    )
    .unwrap();

    let output = run_helper(&markdown, &root, &marker, "");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "rendered 1 cached 0 failed 1"
    );
    assert!(output.status.success());
    assert_eq!(cached_svgs(&root).len(), 1);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn should_render_nothing_when_markdown_has_no_diagrams() {
    let root = private_dir("none");
    let markdown = root.join("plain.md");
    fs::write(&markdown, "# Plain\n\nNo diagrams.\n").unwrap();

    let output = run_helper(&markdown, &root, "none", "");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "rendered 0 cached 0 failed 0"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn should_leave_cache_empty_when_mermaid_is_disabled_in_config() {
    let root = private_dir("disabled");
    let output = run_helper(
        Path::new(FIXTURE),
        &root,
        "disabled",
        "[mermaid]\nenabled = false\n",
    );
    assert!(output.status.success());
    assert!(cached_svgs(&root).is_empty());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn should_fail_with_a_message_when_the_file_is_missing() {
    let root = private_dir("missing");
    let output = run_helper(&root.join("absent.md"), &root, "missing", "");
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn should_fail_with_a_message_when_config_value_is_invalid() {
    let root = private_dir("badconfig");
    let output = run_helper(
        Path::new(FIXTURE),
        &root,
        "badconfig",
        "[mermaid]\ntimeout_seconds = 0\n",
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("timeout_seconds"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn should_fail_with_usage_when_the_file_argument_is_missing() {
    let output = Command::new(env!("CARGO_BIN_EXE_limpid-host"))
        .arg("--render-mermaid")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage"));
}

#[test]
fn should_render_without_any_window_when_helper_window_is_never() {
    if !display_available() {
        eprintln!("skipped: no display");
        return;
    }
    let root = private_dir("never");
    let marker = format!("never-{}", std::process::id());
    let output = run_helper(
        Path::new(FIXTURE),
        &root,
        &marker,
        "[mermaid]\nhelper_window = \"never\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "rendered 2 cached 0 failed 0",
        "stderr {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(cached_svgs(&root).len(), 2);
    let _ = fs::remove_dir_all(&root);
}
