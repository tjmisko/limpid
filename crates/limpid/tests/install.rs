//! `install.sh --dry-run` shows what each mode would do without doing it.

use std::{path::PathBuf, process::Command};

fn install_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../install.sh")
}

fn dry_run(webkit: Option<&str>) -> (bool, String, String) {
    let mut command = Command::new("bash");
    command
        .arg(install_script())
        .arg("--dry-run")
        .env("PREFIX", "/nonexistent/prefix")
        .env_remove("LIMPID_WEBKIT");
    if let Some(value) = webkit {
        command.env("LIMPID_WEBKIT", value);
    }
    let output = command.output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn should_install_everything_but_the_host_when_webkit_is_not_asked_for() {
    let (ok, stdout, stderr) = dry_run(None);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("crates/limpid-core"), "{stdout}");
    assert!(stdout.contains("crates/limpid-view"), "{stdout}");
    assert!(stdout.contains("/crates/limpid --root"), "{stdout}");
    assert!(!stdout.contains("limpid-host"), "{stdout}");
    assert!(!stdout.contains("webkit"), "{stdout}");
    assert!(stdout.contains("grammars sync"), "{stdout}");
}

#[test]
fn should_build_the_host_with_the_webkit_feature_when_asked() {
    let (ok, stdout, stderr) = dry_run(Some("1"));
    assert!(ok, "{stderr}");
    let host_line = stdout
        .lines()
        .find(|line| line.contains("crates/limpid-host"))
        .unwrap_or_else(|| panic!("no host install in {stdout}"));
    assert!(host_line.contains("--features webkit"), "{host_line}");
}

#[test]
fn should_treat_zero_as_no_webkit_when_the_variable_is_set_to_zero() {
    let (ok, stdout, _) = dry_run(Some("0"));
    assert!(ok);
    assert!(!stdout.contains("limpid-host"));
}

#[test]
fn should_reject_other_webkit_values_when_the_variable_is_malformed() {
    let (ok, _, stderr) = dry_run(Some("yes"));
    assert!(!ok);
    assert!(stderr.contains("LIMPID_WEBKIT"), "{stderr}");
}

#[test]
fn should_say_nothing_was_installed_when_dry_running() {
    let (ok, stdout, _) = dry_run(None);
    assert!(ok);
    assert!(
        stdout.contains("Dry run: nothing was installed"),
        "{stdout}"
    );
    assert!(!stdout.contains("Installed limpid"), "{stdout}");
    assert!(!PathBuf::from("/nonexistent/prefix").exists());
}

#[test]
fn should_refuse_unknown_options() {
    let output = Command::new("bash")
        .arg(install_script())
        .arg("--wat")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option"));
}
