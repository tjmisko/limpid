//! Stage 2: the real `limpid --viewer view` launcher, run against a page the
//! bridge importer wrote, hands the imported Markdown to the native viewer.

use limpid_core::{
    bridge::{Delivery, OpenPage},
    bridge_import::{Importer, LauncherOpener, import_page},
    transport::{Handoff, Scheme, view_socket_path},
};
use std::{
    fs,
    io::Read,
    os::unix::net::UnixListener,
    path::PathBuf,
    thread::JoinHandle,
    time::{Duration, Instant},
};

fn receive_view_handoff(socket: PathBuf) -> JoinHandle<Option<Handoff>> {
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    stream.read_to_end(&mut bytes).unwrap();
                    if !bytes.is_empty() {
                        return Some(serde_json::from_slice::<Handoff>(&bytes).unwrap());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("socket accept failed: {error}"),
            }
        }
        None
    })
}

#[test]
fn should_hand_the_imported_markdown_to_the_native_viewer_when_the_real_launcher_opens_it() {
    let directory = std::env::temp_dir().join(format!("limpid-real-import-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    let cache = directory.join("remote-cache");
    let markdown =
        "# Remote plan\r\n\r\nnaïve 日本語\r\n\r\n## Questions\r\n\r\n1. Which?\r\n   - A\r\n";
    let page = OpenPage {
        route: "r1".into(),
        session: "sess-a".into(),
        page_id: "page-1".into(),
        instance: "i1".into(),
        remote_path: Some("/nowhere/on/this/host/page.md".into()),
        markdown: markdown.into(),
        annotations: serde_json::json!({"replies": {"q1": "A"}}),
        revision: 0,
        delivery: Delivery::Remote,
    };
    let imported = import_page(&cache, &page).unwrap();
    assert!(!imported.page.exists(), "the .html path need not exist");

    let receiver = receive_view_handoff(view_socket_path(&directory, Scheme::Dark));
    // The launcher inherits these; the process-wide environment is not touched.
    let launcher = std::process::Command::new(env!("CARGO_BIN_EXE_limpid"))
        .args(["--viewer", "view", "--dark"])
        .arg(&imported.page)
        .env("XDG_RUNTIME_DIR", &directory)
        .env("XDG_CACHE_HOME", &directory)
        .env("XDG_CONFIG_HOME", &directory)
        .output()
        .unwrap();
    assert!(
        launcher.status.success(),
        "{}",
        String::from_utf8_lossy(&launcher.stderr)
    );
    let request = receiver.join().unwrap().expect("no handoff received");
    assert_eq!(request.theme, Scheme::Dark);
    assert_eq!(request.path, imported.markdown);
    assert_eq!(fs::read(&request.path).unwrap(), markdown.as_bytes());
    assert_eq!(request.source, None);
    assert!(!request.source_backed);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn should_open_through_the_launcher_opener_when_the_importer_uses_the_real_binary() {
    let directory = std::env::temp_dir().join(format!("limpid-real-opener-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    // LauncherOpener runs in this process's environment; point the socket
    // lookup at the test directory through a wrapper script.
    let wrapper = directory.join("limpid-wrapper");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nXDG_RUNTIME_DIR={d} XDG_CACHE_HOME={d} XDG_CONFIG_HOME={d} exec {bin} \"$@\"\n",
            d = directory.display(),
            bin = env!("CARGO_BIN_EXE_limpid")
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let receiver = receive_view_handoff(view_socket_path(&directory, Scheme::Light));
    let importer = Importer::new(
        directory.join("cache"),
        Scheme::Light,
        std::sync::Arc::new(LauncherOpener::new(wrapper)),
    );
    use limpid_core::bridge::PageSink;
    importer
        .open(&OpenPage {
            route: "r1".into(),
            session: "s".into(),
            page_id: "p".into(),
            instance: "i".into(),
            remote_path: None,
            markdown: "# Hi\n".into(),
            annotations: serde_json::Value::Null,
            revision: 0,
            delivery: Delivery::Remote,
        })
        .unwrap();
    let request = receiver.join().unwrap().expect("no handoff received");
    assert_eq!(request.theme, Scheme::Light);
    assert_eq!(fs::read_to_string(&request.path).unwrap(), "# Hi\n");
    fs::remove_dir_all(directory).unwrap();
}
