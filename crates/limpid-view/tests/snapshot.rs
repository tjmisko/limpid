//! Headless snapshot tests: no display needed.

use std::{
    fs::File,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.md");
const DARK_BG: [u8; 3] = [0x0c, 0x10, 0x15];
const LIGHT_BG: [u8; 3] = [0xfb, 0xfa, 0xf7];

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("limpid-view-snap-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the binary with a private, empty config directory so the user's own
/// settings cannot change the output.
fn run(args: &[&str], config: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_limpid-view"))
        .args(args)
        .env("XDG_CONFIG_HOME", config)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .output()
        .unwrap()
}

struct Decoded {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl Decoded {
    fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let at = ((y * self.width + x) * 3) as usize;
        [self.rgb[at], self.rgb[at + 1], self.rgb[at + 2]]
    }

    /// Pixels bright enough to be text rather than a background tint.
    fn bright_pixels(&self) -> usize {
        self.rgb
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|pixel| pixel.iter().any(|&channel| channel > 0x80))
            .count()
    }

    fn pixels_differing_from(&self, background: [u8; 3]) -> usize {
        self.rgb
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|pixel| **pixel != background)
            .count()
    }
}

fn decode(path: &Path) -> Decoded {
    let mut reader = png::Decoder::new(File::open(path).unwrap())
        .read_info()
        .unwrap();
    let mut rgb = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgb).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    Decoded {
        width: info.width,
        height: info.height,
        rgb,
    }
}

fn snapshot(name: &str, extra: &[&str]) -> Decoded {
    let dir = scratch(name);
    let out = dir.join("shot.png");
    let mut args = vec!["--snapshot", "800x600", "--out", out.to_str().unwrap()];
    args.extend_from_slice(extra);
    args.push(FIXTURE);
    let output = run(&args, &dir);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decoded = decode(&out);
    std::fs::remove_dir_all(dir).unwrap();
    decoded
}

#[test]
fn should_match_the_requested_size_and_theme_background_when_snapshotting_at_scale_one() {
    let image = snapshot("one", &[]);
    assert_eq!((image.width, image.height), (800, 600));
    assert_eq!(image.pixel(0, 0), DARK_BG);
    assert_eq!(image.pixel(799, 599), DARK_BG);
}

#[test]
fn should_double_the_dimensions_when_snapshotting_at_scale_two() {
    let image = snapshot("two", &["--scale", "2"]);
    assert_eq!((image.width, image.height), (1600, 1200));
    assert_eq!(image.pixel(0, 0), DARK_BG);
}

#[test]
fn should_draw_text_pixels_when_the_document_has_content() {
    let image = snapshot("ink", &[]);
    let lit = image.pixels_differing_from(DARK_BG);
    assert!(lit > 500, "only {lit} pixels differ from the background");
}

#[test]
fn should_use_the_light_background_when_light_is_requested() {
    let image = snapshot("light", &["--light"]);
    assert_eq!(image.pixel(0, 0), LIGHT_BG);
}

#[test]
fn should_fail_with_a_message_when_the_target_is_missing() {
    let dir = scratch("missing");
    let out = dir.join("shot.png");
    let output = run(
        &[
            "--snapshot",
            "100x100",
            "--out",
            out.to_str().unwrap(),
            "/nonexistent/file.md",
        ],
        &dir,
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("/nonexistent/file.md"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!out.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_reject_a_bad_configuration_key_when_the_settings_file_is_invalid() {
    let dir = scratch("badconfig");
    std::fs::create_dir_all(dir.join("limpid")).unwrap();
    std::fs::write(dir.join("limpid/config.toml"), "bogus_key = 1\n").unwrap();
    let out = dir.join("shot.png");
    let output = run(
        &[
            "--snapshot",
            "100x100",
            "--out",
            out.to_str().unwrap(),
            FIXTURE,
        ],
        &dir,
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("bogus_key"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_honour_style_font_size_when_the_config_sets_it() {
    let dir = scratch("fontsize");
    std::fs::create_dir_all(dir.join("limpid")).unwrap();
    std::fs::write(dir.join("limpid/config.toml"), "[style]\nfont_size = 30\n").unwrap();
    let big = dir.join("big.png");
    let output = run(
        &[
            "--snapshot",
            "800x600",
            "--out",
            big.to_str().unwrap(),
            FIXTURE,
        ],
        &dir,
    );
    assert!(output.status.success());
    let big = decode(&big);
    let normal = snapshot("fontsize-default", &[]);
    // Larger type puts more bright ink on the page, even though less of the
    // document fits.
    assert!(big.bright_pixels() > normal.bright_pixels());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_not_panic_when_the_document_is_empty_or_binary() {
    let dir = scratch("odd");
    for (name, bytes) in [
        ("empty.md", Vec::new()),
        ("binary.md", vec![0u8, 159, 146, 150, 0xff, b'\n', 0, 0]),
        ("fence.md", b"```\nnever closed".to_vec()),
    ] {
        let source = dir.join(name);
        std::fs::write(&source, bytes).unwrap();
        let out = dir.join(format!("{name}.png"));
        let output = run(
            &[
                "--snapshot",
                "120x90",
                "--out",
                out.to_str().unwrap(),
                source.to_str().unwrap(),
            ],
            &dir,
        );
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let image = decode(&out);
        assert_eq!(image.pixel(0, 0), DARK_BG, "{name}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
