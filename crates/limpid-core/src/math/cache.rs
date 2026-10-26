//! The on-disk math cache: `<root>/<hash>.svg` plus a `<hash>.json` sidecar
//! with `{ width, height, baseline, backend }` in px at 96 dpi. `baseline` is
//! the distance from the top of the SVG down to the text baseline.
//!
//! Writes go to a temporary name in the same directory and are renamed into
//! place, the SVG first and the sidecar last, so a reader that finds a sidecar
//! always finds a complete SVG.

use super::Rendered;
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq)]
pub struct Cache {
    root: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Sidecar {
    width: f32,
    height: f32,
    baseline: f32,
    backend: String,
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `$XDG_CACHE_HOME/limpid/math`, else `~/.cache/limpid/math`.
    pub fn default_root() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
        Some(base.join("limpid").join("math"))
    }

    pub fn open_default() -> Option<Self> {
        Self::default_root().map(Self::new)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn svg_path(&self, hash: &str) -> PathBuf {
        self.root.join(format!("{hash}.svg"))
    }

    fn json_path(&self, hash: &str) -> PathBuf {
        self.root.join(format!("{hash}.json"))
    }

    /// The cached render for `hash`, or `None` when either file is missing or
    /// the sidecar does not parse. A hash that is not plain hex never matches,
    /// so it cannot name a path outside the cache.
    pub fn get(&self, hash: &str) -> Option<Rendered> {
        if !is_plain_hash(hash) {
            return None;
        }
        let svg = self.svg_path(hash);
        if !svg.is_file() {
            return None;
        }
        let sidecar: Sidecar =
            serde_json::from_slice(&std::fs::read(self.json_path(hash)).ok()?).ok()?;
        Some(Rendered {
            svg,
            width: sidecar.width,
            height: sidecar.height,
            baseline: sidecar.baseline,
            backend: sidecar.backend,
        })
    }

    /// Copy the SVG at `rendered.svg` into the cache under `hash` and write the
    /// sidecar. Returns the cached entry.
    pub fn store(&self, hash: &str, rendered: &Rendered) -> io::Result<Rendered> {
        if !is_plain_hash(hash) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not a formula hash: {hash:?}"),
            ));
        }
        std::fs::create_dir_all(&self.root)?;
        let svg = std::fs::read(&rendered.svg)?;
        let stored = Rendered {
            svg: self.svg_path(hash),
            ..rendered.clone()
        };
        self.write_atomic(&stored.svg, &svg)?;
        let sidecar = serde_json::to_vec(&Sidecar {
            width: stored.width,
            height: stored.height,
            baseline: stored.baseline,
            backend: stored.backend.clone(),
        })
        .map_err(io::Error::other)?;
        self.write_atomic(&self.json_path(hash), &sidecar)?;
        Ok(stored)
    }

    fn write_atomic(&self, destination: &Path, bytes: &[u8]) -> io::Result<()> {
        let name = destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temporary = self
            .root
            .join(format!(".{name}.{}.tmp", std::process::id()));
        std::fs::write(&temporary, bytes)?;
        match std::fs::rename(&temporary, destination) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                Err(error)
            }
        }
    }
}

fn is_plain_hash(hash: &str) -> bool {
    !hash.is_empty() && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limpid-math-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(svg: PathBuf) -> Rendered {
        Rendered {
            svg,
            width: 12.5,
            height: 9.0,
            baseline: 7.0,
            backend: "latex".into(),
        }
    }

    #[test]
    fn should_round_trip_entry_when_stored() {
        let dir = scratch("roundtrip");
        let source = dir.join("in.svg");
        std::fs::write(&source, "<svg/>").unwrap();
        let cache = Cache::new(dir.join("cache"));
        let stored = cache.store("0123456789abcdef", &sample(source)).unwrap();
        assert_eq!(cache.get("0123456789abcdef"), Some(stored));
        assert_eq!(
            std::fs::read_to_string(cache.svg_path("0123456789abcdef")).unwrap(),
            "<svg/>"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_miss_when_sidecar_is_absent_or_corrupt() {
        let dir = scratch("miss");
        let cache = Cache::new(&dir);
        assert_eq!(cache.get("abc123"), None);
        std::fs::write(dir.join("abc123.svg"), "<svg/>").unwrap();
        assert_eq!(cache.get("abc123"), None);
        std::fs::write(dir.join("abc123.json"), "{not json").unwrap();
        assert_eq!(cache.get("abc123"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_refuse_hash_when_it_could_escape_the_cache() {
        let dir = scratch("escape");
        let cache = Cache::new(dir.join("cache"));
        let source = dir.join("in.svg");
        std::fs::write(&source, "<svg/>").unwrap();
        assert!(cache.store("../evil", &sample(source)).is_err());
        assert_eq!(cache.get("../evil"), None);
        assert_eq!(cache.get(""), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_leave_no_temporary_files_when_stored() {
        let dir = scratch("tmpfiles");
        let source = dir.join("in.svg");
        std::fs::write(&source, "<svg/>").unwrap();
        let cache = Cache::new(dir.join("cache"));
        cache.store("aa", &sample(source)).unwrap();
        let names: Vec<_> = std::fs::read_dir(cache.root())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|name| !name.ends_with(".tmp")),
            "{names:?}"
        );
        assert_eq!(names.len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_fail_when_source_svg_is_missing() {
        let dir = scratch("nosource");
        let cache = Cache::new(dir.join("cache"));
        assert!(cache.store("aa", &sample(dir.join("nope.svg"))).is_err());
        assert_eq!(cache.get("aa"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
