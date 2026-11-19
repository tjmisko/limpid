//! `limpid bridge serve`: the local hub that plain `ssh` logins forward to.
//!
//! One long-lived hub serves every login. Each login forwards it with a line
//! in `~/.ssh/config`:
//!
//! ```text
//! RemoteForward /tmp/limpid-%r.sock ${XDG_RUNTIME_DIR}/limpid/hub.sock
//! ```
//!
//! and `limpid` on the remote finds that socket by path. Every connection
//! announces its own route, so logins stay apart on one hub exactly as they
//! did with one hub each.
//!
//! The hub socket may come from systemd socket activation (`LISTEN_FDS`), so
//! nothing runs until the first page arrives; otherwise the hub binds it. The
//! save socket the viewer pushes feedback to is always bound here: it is only
//! used for pages this process opened.

use crate::{
    bridge::Hub,
    bridge_import::{Importer, LauncherOpener},
    bridge_push::PushService,
    bridge_ssh::{cache_dir, private_directory},
    transport::Scheme,
};
use std::{
    ffi::OsString,
    io::ErrorKind,
    os::{
        fd::{FromRawFd, RawFd},
        unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::Arc,
};

/// The hub socket's name in the private runtime directory.
pub const HUB_SOCKET: &str = "hub.sock";
/// The save socket's name in the private runtime directory.
pub const SAVE_SOCKET: &str = "save.sock";
/// The first descriptor systemd passes (`SD_LISTEN_FDS_START`).
const FIRST_PASSED_FD: RawFd = 3;

pub fn usage() -> &'static str {
    "usage: limpid bridge serve [--dark|--light]"
}

/// The theme pages open in. Only `serve` is a subcommand.
pub fn parse_args(args: &[OsString]) -> Result<Scheme, String> {
    let mut rest = args.iter().map(|arg| arg.to_string_lossy());
    match rest.next().as_deref() {
        Some("serve") => {}
        Some("--help" | "-h") | None => return Err(usage().into()),
        Some(other) => return Err(format!("unknown bridge command {other:?}; {}", usage())),
    }
    let mut scheme = Scheme::Dark;
    for arg in rest {
        match arg.as_ref() {
            "--dark" => scheme = Scheme::Dark,
            "--light" => scheme = Scheme::Light,
            other => return Err(format!("unknown option {other:?}; {}", usage())),
        }
    }
    Ok(scheme)
}

/// The hub socket systemd passed to this process, if any. The variables are
/// for this process only when `LISTEN_PID` names it.
pub fn activated_fd(listen_pid: Option<&str>, listen_fds: Option<&str>, pid: u32) -> Option<RawFd> {
    let listen_pid: u32 = listen_pid?.trim().parse().ok()?;
    let listen_fds: u32 = listen_fds?.trim().parse().ok()?;
    (listen_pid == pid && listen_fds >= 1).then_some(FIRST_PASSED_FD)
}

/// Bind a private (0600) socket at `path`, replacing a socket nobody listens
/// on. A live listener or a file that is not a socket is an error: this
/// never takes over another hub or deletes something it did not make.
pub fn bind_fresh(path: &Path) -> Result<UnixListener, String> {
    let shown = path.display();
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(format!(
                    "{shown}: another limpid bridge is already serving here"
                ));
            }
            let is_socket = std::fs::symlink_metadata(path)
                .map(|metadata| metadata.file_type().is_socket())
                .unwrap_or(false);
            if !is_socket {
                return Err(format!(
                    "{shown} exists and is not a socket; not replacing it"
                ));
            }
            std::fs::remove_file(path).map_err(|error| format!("{shown}: {error}"))?;
            UnixListener::bind(path).map_err(|error| format!("{shown}: {error}"))?
        }
        Err(error) => return Err(format!("{shown}: {error}")),
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("{shown}: {error}"))?;
    Ok(listener)
}

/// Mark a passed descriptor close-on-exec so the viewers this hub starts do
/// not inherit the listening socket.
fn close_on_exec(fd: RawFd) -> Result<(), String> {
    // SAFETY: fcntl on a descriptor number has no memory-safety requirements.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(format!(
            "the socket systemd passed (fd {fd}) is not usable: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// The hub and save socket paths under the private runtime directory.
pub fn socket_paths(runtime: Option<OsString>) -> Result<(PathBuf, PathBuf), String> {
    let directory = private_directory(runtime)?;
    Ok((directory.join(HUB_SOCKET), directory.join(SAVE_SOCKET)))
}

/// Serve until killed. `limpid` is the program the importer launches to open
/// each page.
pub fn run(args: &[OsString], limpid: &Path) -> Result<(), String> {
    let scheme = parse_args(args)?;
    let activated = activated_fd(
        std::env::var("LISTEN_PID").ok().as_deref(),
        std::env::var("LISTEN_FDS").ok().as_deref(),
        std::process::id(),
    );
    // SAFETY: called before this process starts any thread. The variables
    // describe descriptors meant for this process, not for its children.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    let (hub_path, save_path) = socket_paths(std::env::var_os("XDG_RUNTIME_DIR"))?;
    let listener = match activated {
        Some(fd) => {
            close_on_exec(fd)?;
            // SAFETY: systemd passed this listening socket to this process
            // (LISTEN_PID matched) and nothing else in it owns the descriptor.
            unsafe { UnixListener::from_raw_fd(fd) }
        }
        None => bind_fresh(&hub_path)?,
    };
    let importer = Arc::new(
        Importer::new(
            cache_dir()?,
            scheme,
            Arc::new(LauncherOpener::new(limpid.to_path_buf())),
        )
        .with_save_socket(save_path.clone()),
    );
    let hub = Arc::new(Hub::from_listener(listener, &hub_path, importer.clone()));
    let _pushes = PushService::from_listener(bind_fresh(&save_path)?, &save_path, importer, hub);
    eprintln!("limpid: bridge hub serving {}", hub_path.display());
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limpid-serve-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn should_take_the_theme_when_serve_is_given_a_scheme_flag() {
        assert_eq!(parse_args(&args(&["serve"])), Ok(Scheme::Dark));
        assert_eq!(parse_args(&args(&["serve", "--light"])), Ok(Scheme::Light));
    }

    #[test]
    fn should_refuse_when_the_subcommand_or_an_option_is_unknown() {
        assert!(parse_args(&args(&[])).unwrap_err().contains("usage"));
        assert!(parse_args(&args(&["start"])).unwrap_err().contains("start"));
        assert!(
            parse_args(&args(&["serve", "--port"]))
                .unwrap_err()
                .contains("--port")
        );
    }

    #[test]
    fn should_use_the_first_passed_fd_when_systemd_names_this_process() {
        assert_eq!(activated_fd(Some("42"), Some("1"), 42), Some(3));
        assert_eq!(activated_fd(Some("42"), Some("2"), 42), Some(3));
    }

    #[test]
    fn should_ignore_activation_when_the_variables_are_absent_malformed_or_for_another_process() {
        assert_eq!(activated_fd(None, None, 42), None);
        assert_eq!(activated_fd(Some("42"), None, 42), None);
        assert_eq!(activated_fd(Some("41"), Some("1"), 42), None);
        assert_eq!(activated_fd(Some("42"), Some("0"), 42), None);
        assert_eq!(activated_fd(Some("x"), Some("1"), 42), None);
        assert_eq!(activated_fd(Some("42"), Some("-1"), 42), None);
    }

    #[test]
    fn should_bind_a_private_socket_when_nothing_is_there() {
        let dir = scratch("fresh");
        let path = dir.join("hub.sock");
        let _listener = bind_fresh(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_replace_a_socket_when_nobody_listens_on_it() {
        let dir = scratch("stale");
        let path = dir.join("hub.sock");
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "a dropped listener leaves its file behind");
        let listener = bind_fresh(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let _client = UnixStream::connect(&path).unwrap();
        assert!(
            listener.accept().is_ok(),
            "the new socket is the one listening"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_refuse_to_take_over_when_another_hub_is_listening() {
        let dir = scratch("live");
        let path = dir.join("hub.sock");
        let _first = bind_fresh(&path).unwrap();
        let error = bind_fresh(&path).unwrap_err();
        assert!(error.contains("already serving"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_leave_a_file_alone_when_it_is_not_a_socket() {
        let dir = scratch("file");
        let path = dir.join("hub.sock");
        std::fs::write(&path, "keep me").unwrap();
        let error = bind_fresh(&path).unwrap_err();
        assert!(error.contains("not a socket"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
