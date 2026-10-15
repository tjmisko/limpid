# Phase 0 measurement notes

The user deprioritized benchmarking on 2026-10-15 in favor of building the product. Phase 0 measurements are deferred.

## Environment checked on 2026-10-15

- `pkg-config --modversion gtk4 webkitgtk-6.0` cannot find either development package on this machine. The GTK/WebKit Rust window spike and its cold-start and memory comparison (P0.2) cannot run until those packages are available.
- The planned source directory `~/.cache/htmlview-bench/` is unavailable in the current filesystem view. The benchmark scripts and htmlview baseline (P0.1) still need to be recovered or recreated.
- No performance numbers are claimed yet. P0.3–P0.5 and the design's corresponding open questions remain open.
