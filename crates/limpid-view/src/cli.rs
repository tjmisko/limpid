//! Command-line parsing for `limpid-view`.

use crate::snapshot::parse_size;
use limpid_core::transport::Scheme;
use std::path::PathBuf;

pub const USAGE: &str = "usage: limpid-view [--dark|--light] [--separate] [--socket PATH] \
[--snapshot WxH [--scale S] --out FILE.png] TARGET";

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub out: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    pub scheme: Scheme,
    pub separate: bool,
    pub socket: Option<PathBuf>,
    pub snapshot: Option<Snapshot>,
    pub target: Option<PathBuf>,
}

pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut scheme = Scheme::Dark;
    let mut separate = false;
    let mut socket = None;
    let mut size = None;
    let mut scale = None;
    let mut out = None;
    let mut target = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| {
            arguments
                .next()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match argument.as_str() {
            "--dark" => scheme = Scheme::Dark,
            "--light" => scheme = Scheme::Light,
            "--separate" => separate = true,
            "--socket" => socket = Some(PathBuf::from(value("--socket")?)),
            "--snapshot" => {
                let text = value("--snapshot")?;
                size = Some(
                    parse_size(&text)
                        .ok_or_else(|| format!("--snapshot wants WIDTHxHEIGHT, got {text:?}"))?,
                );
            }
            "--scale" => {
                let text = value("--scale")?;
                let parsed: f32 = text
                    .parse()
                    .ok()
                    .filter(|scale: &f32| scale.is_finite() && *scale > 0.0)
                    .ok_or_else(|| format!("--scale wants a positive number, got {text:?}"))?;
                scale = Some(parsed);
            }
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            flag if flag.starts_with("--") => return Err(format!("unknown option: {flag}")),
            _ if target.is_some() => return Err(format!("unexpected argument: {argument}")),
            _ => target = Some(PathBuf::from(argument)),
        }
    }
    let snapshot = match (size, out) {
        (Some((width, height)), Some(out)) => Some(Snapshot {
            width,
            height,
            scale: scale.unwrap_or(1.0),
            out,
        }),
        (Some(_), None) => return Err("--snapshot needs --out FILE.png".into()),
        (None, Some(_)) => return Err("--out needs --snapshot WxH".into()),
        (None, None) if scale.is_some() => return Err("--scale needs --snapshot WxH".into()),
        (None, None) => None,
    };
    if snapshot.is_some() && target.is_none() {
        return Err("--snapshot needs a TARGET file".into());
    }
    if snapshot.is_some() && socket.is_some() {
        return Err("--socket does not apply to --snapshot".into());
    }
    Ok(Args {
        scheme,
        separate,
        socket,
        snapshot,
        target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_words(line: &str) -> Result<Args, String> {
        parse(line.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn should_default_to_dark_with_no_target_when_given_no_arguments() {
        let args = parse_words("").unwrap();
        assert_eq!(args.scheme, Scheme::Dark);
        assert!(!args.separate && args.snapshot.is_none() && args.target.is_none());
    }

    #[test]
    fn should_parse_a_snapshot_request_when_all_parts_are_present() {
        let args =
            parse_words("--light --snapshot 800x600 --scale 2 --out /tmp/a.png notes.md").unwrap();
        assert_eq!(args.scheme, Scheme::Light);
        assert_eq!(
            args.snapshot,
            Some(Snapshot {
                width: 800,
                height: 600,
                scale: 2.0,
                out: "/tmp/a.png".into()
            })
        );
        assert_eq!(args.target, Some("notes.md".into()));
    }

    #[test]
    fn should_default_scale_to_one_when_not_given() {
        let args = parse_words("--snapshot 10x10 --out o.png n.md").unwrap();
        assert_eq!(args.snapshot.unwrap().scale, 1.0);
    }

    #[test]
    fn should_reject_incomplete_or_inconsistent_snapshot_options() {
        for line in [
            "--snapshot 800x600 n.md",
            "--out o.png n.md",
            "--scale 2 n.md",
            "--snapshot 800x600 --out o.png",
            "--snapshot 800x600 --out o.png --socket s n.md",
            "--snapshot",
            "--snapshot big --out o.png n.md",
            "--snapshot 1x1 --scale 0 --out o.png n.md",
            "--snapshot 1x1 --scale nan --out o.png n.md",
            "--snapshot 1x1 --scale -1 --out o.png n.md",
        ] {
            assert!(parse_words(line).is_err(), "{line}");
        }
    }

    #[test]
    fn should_reject_unknown_flags_and_extra_targets() {
        assert!(parse_words("--wat").unwrap_err().contains("--wat"));
        assert!(parse_words("a.md b.md").unwrap_err().contains("b.md"));
    }

    #[test]
    fn should_take_the_socket_path_when_given() {
        let args = parse_words("--socket /run/x.sock --separate").unwrap();
        assert_eq!(args.socket, Some("/run/x.sock".into()));
        assert!(args.separate);
    }
}
