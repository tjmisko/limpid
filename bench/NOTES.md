# Phase 0 measurement notes

The user deprioritized benchmarking on 2026-10-15 in favor of building the product. Phase 0 measurements are deferred.

## Environment checked on 2026-10-15

- `pkg-config --modversion gtk4 webkitgtk-6.0` reports GTK4 4.22.5 and WebKitGTK 6.0 2.54.1. After clearing Cargo artifacts built with `DOCS_RS=1`, `cargo build --workspace --offline` links all three Rust binaries successfully. `cargo test --workspace --offline` and `cargo clippy --workspace --all-targets --offline -- -D warnings` pass. The P0.2 cold-start and memory comparison remains deferred.
- The planned source directory `~/.cache/htmlview-bench/` is unavailable in the current filesystem view. The benchmark scripts and htmlview baseline (P0.1) still need to be recovered or recreated.
- No performance numbers are claimed yet. P0.3–P0.5 and the design's corresponding open questions remain open.
