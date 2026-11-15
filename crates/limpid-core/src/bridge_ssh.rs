//! `limpid ssh HOST`: the local launcher for a direct SSH login.
//!
//! The launcher owns the local end of a bridge. It binds a private hub socket,
//! runs one OpenSSH client with a fresh reverse Unix-socket forward to that
//! socket, and starts the remote login shell with the route in its
//! environment (so the server needs no `AcceptEnv`). Pages the remote sends
//! arrive on the hub and open in the local native viewer. When the login
//! ends the launcher closes the hub and removes only its own socket.
//!
//! The SSH transport is this one client process: connection sharing is
//! switched off, so the forward lives and dies with this login and never
//! attaches to a lingering master.

use crate::{
    bridge::Hub,
    bridge_import::{Importer, LauncherOpener},
    bridge_push::PushService,
    bridge_relay::{ROUTE_ENV, SOCKET_ENV},
    transport::Scheme,
};
use std::{
    ffi::OsString,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

/// Unix socket paths hold at most 108 bytes including the terminator.
const SOCKET_PATH_LIMIT: usize = 104;

/// ssh options that take a value as the next argument.
const OPTIONS_WITH_VALUE: &str = "BbcDEeFIiJLlmoPpQRSWw";
/// ssh options that would change what this launcher relies on.
const REFUSED_OPTIONS: &str = "NfWsOMTnVGt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshArgs {
    pub scheme: Scheme,
    /// Options for the ssh client, kept in order.
    pub options: Vec<OsString>,
    pub host: OsString,
}

pub fn usage() -> &'static str {
    "usage: limpid ssh [--dark|--light] [ssh options] HOST"
}

pub fn parse_args(args: &[OsString]) -> Result<SshArgs, String> {
    let mut scheme = Scheme::Dark;
    let mut options = Vec::new();
    let mut host = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let text = arg.to_string_lossy();
        if host.is_some() {
            return Err(format!(
                "unexpected argument {text:?} after the host; `limpid ssh` starts an \
                 interactive login and takes no remote command"
            ));
        }
        match text.as_ref() {
            "--dark" => scheme = Scheme::Dark,
            "--light" => scheme = Scheme::Light,
            "--help" | "-h" => return Err(usage().into()),
            option if option.starts_with("--") => {
                return Err(format!("unknown option {option:?}; {}", usage()));
            }
            option if option.starts_with('-') && option.len() > 1 => {
                let letters: Vec<char> = option[1..].chars().collect();
                // The first value-taking letter ends the cluster; its value is
                // the rest of this argument, or else the next one.
                let takes_value = letters.iter().position(|c| OPTIONS_WITH_VALUE.contains(*c));
                let flags = &letters[..takes_value.map_or(letters.len(), |at| at + 1)];
                if let Some(bad) = flags.iter().find(|c| REFUSED_OPTIONS.contains(**c)) {
                    return Err(format!(
                        "ssh option -{bad} is not supported by `limpid ssh`; it needs an \
                         interactive login with its own forwarding"
                    ));
                }
                options.push(arg.clone());
                if let Some(position) = takes_value
                    && position + 1 == letters.len()
                {
                    let value = rest
                        .next()
                        .ok_or_else(|| format!("ssh option {text} requires a value"))?;
                    options.push(value.clone());
                }
            }
            _ if text.is_empty() => return Err("the host is empty".into()),
            _ => host = Some(arg.clone()),
        }
    }
    let host = host.ok_or_else(|| format!("missing host; {}", usage()))?;
    Ok(SshArgs {
        scheme,
        options,
        host,
    })
}

/// Quote one word for a POSIX-style shell (and fish): single quotes, with an
/// embedded quote written as `'\''`.
pub fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The command the remote account's shell runs for the login. It sets the
/// route for the login shell and removes this login's socket when the shell
/// exits. Written to parse the same in sh, bash, zsh and fish: only `env`,
/// single quotes and one script word.
pub fn remote_command(remote_socket: &str, route: &str) -> String {
    let script = "s=$1; \"${SHELL:-/bin/sh}\" -l; c=$?; rm -f \"$s\"; exit $c";
    format!(
        "env {socket_var}={socket} {route_var}={route} sh -c {script} sh {socket}",
        socket_var = SOCKET_ENV,
        route_var = ROUTE_ENV,
        socket = shell_quote(remote_socket),
        route = shell_quote(route),
        script = shell_quote(script),
    )
}

/// The ssh invocation for one login.
pub fn ssh_command(
    program: &Path,
    args: &SshArgs,
    local_socket: &Path,
    remote_socket: &str,
    route: &str,
) -> Result<Command, String> {
    let local = local_socket
        .to_str()
        .ok_or("the local bridge socket path is not valid UTF-8")?;
    // -R splits on ':' and does not quote whitespace.
    if local.contains(':') || local.contains(char::is_whitespace) {
        return Err(format!(
            "the local bridge socket path {local:?} contains ':' or whitespace, which ssh \
             forwarding cannot express; set XDG_RUNTIME_DIR to a plain path"
        ));
    }
    let mut command = Command::new(program);
    command
        .arg("-t")
        .args(["-o", "ExitOnForwardFailure=yes"])
        .args(["-o", "ControlMaster=no"])
        .args(["-o", "ControlPath=none"])
        .arg("-R")
        .arg(format!("{remote_socket}:{local}"))
        .args(&args.options)
        .arg(&args.host)
        .arg(remote_command(remote_socket, route));
    Ok(command)
}

/// A fresh, unguessable login route: 64 random bits as hex.
pub fn new_route() -> Result<String, String> {
    let mut bytes = [0u8; 8];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("could not generate a route: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn remote_socket_for(route: &str) -> String {
    format!("/tmp/limpid-{route}.sock")
}

/// The private directory that holds local bridge sockets.
fn private_directory(runtime: Option<OsString>) -> Result<PathBuf, String> {
    let directory = match runtime.filter(|value| !value.is_empty()) {
        Some(runtime) => PathBuf::from(runtime).join("limpid"),
        None => PathBuf::from(format!("/tmp/limpid-{}", unsafe { libc::getuid() })),
    };
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let metadata = std::fs::metadata(&directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    if metadata.uid() != unsafe { libc::getuid() } {
        return Err(format!(
            "{} is owned by another user; refusing to place a bridge socket there",
            directory.display()
        ));
    }
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    Ok(directory)
}

pub fn local_socket_for(directory: &Path, route: &str) -> Result<PathBuf, String> {
    let path = directory.join(format!("b-{route}.sock"));
    if path.as_os_str().len() > SOCKET_PATH_LIMIT {
        return Err(format!(
            "the bridge socket path {} is longer than {SOCKET_PATH_LIMIT} bytes, the limit \
             for Unix sockets; set XDG_RUNTIME_DIR to a shorter directory",
            path.display()
        ));
    }
    Ok(path)
}

/// The socket the viewer asks to push saved feedback; private like the hub's.
pub fn save_socket_for(directory: &Path, route: &str) -> Result<PathBuf, String> {
    let path = directory.join(format!("s-{route}.sock"));
    if path.as_os_str().len() > SOCKET_PATH_LIMIT {
        return Err(format!(
            "the save socket path {} is longer than {SOCKET_PATH_LIMIT} bytes, the limit \
             for Unix sockets; set XDG_RUNTIME_DIR to a shorter directory",
            path.display()
        ));
    }
    Ok(path)
}

fn cache_dir() -> Result<PathBuf, String> {
    if let Some(base) = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(base).join("limpid/remote"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is required")?;
    Ok(PathBuf::from(home).join(".cache/limpid/remote"))
}

/// Run one login. Returns ssh's exit status for the caller to propagate.
/// `limpid` is the program the importer launches to open each page.
pub fn run(args: &[OsString], ssh: &Path, limpid: &Path) -> Result<i32, String> {
    let args = parse_args(args)?;
    let route = new_route()?;
    let directory = private_directory(std::env::var_os("XDG_RUNTIME_DIR"))?;
    let local_socket = local_socket_for(&directory, &route)?;
    let remote_socket = remote_socket_for(&route);
    let mut command = ssh_command(ssh, &args, &local_socket, &remote_socket, &route)?;
    let save_socket = save_socket_for(&directory, &route)?;
    let importer = Arc::new(
        Importer::new(
            cache_dir()?,
            args.scheme,
            Arc::new(LauncherOpener::new(limpid.to_path_buf())),
        )
        .with_save_socket(save_socket.clone()),
    );
    // Bound before ssh starts so the forward has something to reach. The hub
    // removes the socket it created when shut down, on every path out.
    let hub = Arc::new(
        Hub::bind(&local_socket, importer.clone())
            .map_err(|error| format!("could not bind {}: {error}", local_socket.display()))?,
    );
    let pushes = PushService::bind(&save_socket, importer, hub.clone())
        .map_err(|error| format!("could not bind {}: {error}", save_socket.display()))?;
    let outcome = command.status();
    // Explicit: the save service holds the hub, so dropping alone would not
    // remove the sockets on the error paths below.
    pushes.shutdown();
    hub.shutdown();
    let status = match outcome {
        Ok(status) => status,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{} was not found; install OpenSSH or set LIMPID_SSH",
                ssh.display()
            ));
        }
        Err(error) => return Err(format!("could not run {}: {error}", ssh.display())),
    };
    let code = status.code().unwrap_or(255);
    if code == 255 {
        eprintln!(
            "limpid: ssh failed (exit 255). If it reported a failed remote forward, the server \
             may forbid stream-local forwarding (AllowStreamLocalForwarding) or /tmp is not writable."
        );
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn should_take_the_host_and_keep_options_in_order_when_parsing() {
        let parsed = parse_args(&args(&["-p", "2222", "-i", "key", "--light", "me@box"])).unwrap();
        assert_eq!(parsed.host, OsString::from("me@box"));
        assert_eq!(parsed.options, args(&["-p", "2222", "-i", "key"]));
        assert_eq!(parsed.scheme, Scheme::Light);
    }

    #[test]
    fn should_keep_an_attached_option_value_when_it_shares_the_argument() {
        let parsed = parse_args(&args(&["-p2222", "-oUser=x", "-v", "box"])).unwrap();
        assert_eq!(parsed.options, args(&["-p2222", "-oUser=x", "-v"]));
        assert_eq!(parsed.host, OsString::from("box"));
    }

    #[test]
    fn should_not_mistake_an_option_value_for_the_host_when_parsing() {
        let parsed = parse_args(&args(&["-o", "StrictHostKeyChecking=no", "box"])).unwrap();
        assert_eq!(parsed.host, OsString::from("box"));
        assert_eq!(parsed.options.len(), 2);
    }

    #[test]
    fn should_refuse_when_the_host_is_missing() {
        assert!(parse_args(&args(&[])).unwrap_err().contains("missing host"));
        assert!(
            parse_args(&args(&["-p", "22"]))
                .unwrap_err()
                .contains("missing host")
        );
        assert!(parse_args(&args(&["-p"])).unwrap_err().contains("requires"));
    }

    #[test]
    fn should_refuse_a_remote_command_when_arguments_follow_the_host() {
        let error = parse_args(&args(&["box", "ls"])).unwrap_err();
        assert!(error.contains("no remote command"), "{error}");
    }

    #[test]
    fn should_refuse_options_that_change_the_login_when_parsing() {
        for option in ["-N", "-f", "-W", "-T", "-vN"] {
            let error = parse_args(&args(&[option, "box"])).unwrap_err();
            assert!(error.contains("not supported"), "{option}: {error}");
        }
    }

    #[test]
    fn should_refuse_unknown_long_options_and_empty_hosts() {
        assert!(parse_args(&args(&["--bogus", "box"])).is_err());
        assert!(parse_args(&args(&[""])).is_err());
    }

    #[test]
    fn should_quote_embedded_quotes_and_metacharacters_when_building_shell_words() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("a b;$(rm -rf /)`x`"), "'a b;$(rm -rf /)`x`'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn should_set_the_route_in_the_login_environment_when_building_the_remote_command() {
        let command = remote_command("/tmp/limpid-ab.sock", "ab");
        assert!(command.starts_with("env "), "{command}");
        assert!(command.contains("LIMPID_BRIDGE_SOCKET='/tmp/limpid-ab.sock'"));
        assert!(command.contains("LIMPID_BRIDGE_ROUTE='ab'"));
        assert!(command.contains("rm -f"), "socket cleanup on logout");
    }

    #[test]
    fn should_build_one_ssh_client_with_its_own_forward_when_building_the_command() {
        let parsed = parse_args(&args(&["-p", "2222", "me@box"])).unwrap();
        let command = ssh_command(
            Path::new("ssh"),
            &parsed,
            Path::new("/run/user/1/limpid/b-ab.sock"),
            "/tmp/limpid-ab.sock",
            "ab",
        )
        .unwrap();
        let words: Vec<String> = command
            .get_args()
            .map(|word| word.to_string_lossy().into_owned())
            .collect();
        assert_eq!(command.get_program(), "ssh");
        assert!(words.contains(&"ExitOnForwardFailure=yes".to_string()));
        assert!(words.contains(&"ControlMaster=no".to_string()));
        assert!(words.contains(&"ControlPath=none".to_string()));
        let forward = words.iter().position(|w| w == "-R").unwrap();
        assert_eq!(
            words[forward + 1],
            "/tmp/limpid-ab.sock:/run/user/1/limpid/b-ab.sock"
        );
        let host = words.iter().position(|w| w == "me@box").unwrap();
        assert_eq!(host, words.len() - 2, "the remote command follows the host");
        assert!(words[host - 1] == "2222" && words[host - 2] == "-p");
    }

    #[test]
    fn should_refuse_a_local_socket_path_when_ssh_cannot_express_it() {
        let parsed = parse_args(&args(&["box"])).unwrap();
        for path in ["/run/a:b/s.sock", "/run/a b/s.sock"] {
            let error =
                ssh_command(Path::new("ssh"), &parsed, Path::new(path), "/tmp/r", "r").unwrap_err();
            assert!(error.contains("XDG_RUNTIME_DIR"), "{error}");
        }
    }

    #[test]
    fn should_refuse_a_socket_path_when_it_exceeds_the_unix_limit() {
        let long = PathBuf::from(format!("/{}", "d".repeat(120)));
        let error = local_socket_for(&long, "ab").unwrap_err();
        assert!(error.contains("shorter"), "{error}");
        assert!(local_socket_for(Path::new("/run/user/1000/limpid"), "0123456789abcdef").is_ok());
    }

    #[test]
    fn should_make_distinct_routes_when_asked_repeatedly() {
        let a = new_route().unwrap();
        let b = new_route().unwrap();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn should_create_a_private_directory_when_the_runtime_directory_is_given() {
        let runtime = std::env::temp_dir().join(format!("limpid-ssh-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime);
        std::fs::create_dir_all(&runtime).unwrap();
        let directory = private_directory(Some(runtime.clone().into_os_string())).unwrap();
        let mode = std::fs::metadata(&directory).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        std::fs::remove_dir_all(runtime).unwrap();
    }
}
