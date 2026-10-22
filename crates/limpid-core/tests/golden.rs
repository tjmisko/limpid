use limpid_core::render::{self, RenderMode};

#[test]
fn basic_page_matches_reviewed_html() {
    // The golden stays reproducible whether optional grammars are installed.
    unsafe {
        std::env::set_var(
            "XDG_DATA_HOME",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/no-grammars"),
        );
        // Point config at a missing directory so the golden uses built-in defaults.
        std::env::set_var(
            "XDG_CONFIG_HOME",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/no-config"),
        );
    }
    let source = include_str!("golden/basic.md");
    let actual = render::page(&render::render(source, RenderMode::File));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/basic.html"),
            &actual,
        )
        .unwrap();
        return;
    }
    let expected = include_str!("golden/basic.html");
    if actual != expected {
        let index = actual
            .bytes()
            .zip(expected.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(actual.len().min(expected.len()));
        panic!(
            "first difference at byte {index}: actual {:?}, expected {:?}",
            &actual[index..actual.len().min(index + 150)],
            &expected[index..expected.len().min(index + 150)]
        );
    }
}
