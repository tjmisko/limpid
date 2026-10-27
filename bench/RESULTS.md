# limpid benchmark results

Measured 2026-10-16 on the same machine as the htmlview numbers
(Apple Silicon / Asahi Linux, GTK 4.22.5, WebKitGTK 6.0 2.54.1). Method matches
htmlview's `PERFORMANCE.md`: hyprctl polls for a window whose title the page
sets on `load`; PSS and `Pss_Anon` ("private") come from
`/proc/<pid>/smaps_rollup`, read 2 s after load. Each run uses a private
`XDG_RUNTIME_DIR`, so the limpid-host socket is isolated from the user's host.

- Large page: `bench/sample.html`, 2004 lines, 315 KB.
- Small page: `bench/small.html`, 234 lines, 91 KB.

## Cold start, one window (5 runs, median)

| Viewer (same page, same day) | Load | PSS | Private |
|---|---|---|---|
| limpid | 0.31 s | 173 MiB | 100 MiB |
| htmlview (recorded alone) | 0.38 s | 246 MiB | 114 MiB |
| Chrome, cold | 0.43 s | 439 MiB | 207 MiB |
| Chromium, cold | 0.55 s | 467 MiB | 234 MiB |
| Firefox, cold | 1.01 s | 705 MiB | 538 MiB |

On the large page limpid is 0.35 s / 180 MiB PSS / 110 MiB private.

One-window breakdown (small page): limpid-host 63 PSS / 38 private, web process
85 / 46, network 22 / 15, sandbox helpers ~2 / 2.

## Shared windows, one host (limpid)

Small page (91 KB), each window a distinct file:

| Windows | Open | PSS | Private | Δ PSS | Δ private |
|---|---|---|---|---|---|
| 1 | 0.29 s | 167 MiB | 97 MiB | | |
| 2 | 0.06 s | 177 MiB | 107 MiB | 10 | 10 |
| 3 | 0.05 s | 192 MiB | 122 MiB | 15 | 15 |
| 5 | 0.07 s | 214 MiB | 143 MiB | | |
| 10 | 0.08 s | 267 MiB | 196 MiB | ~10 | ~10 |

Large page (315 KB), each window a distinct file:

| Windows | Open | PSS | Private |
|---|---|---|---|
| 1 | 0.34 s | 181 MiB | 110 MiB |
| 3 | 0.13 s | 224 MiB | 151 MiB |
| 5 | 0.53 s | 299 MiB | 225 MiB |
| 10 | 0.48 s | 446 MiB | 372 MiB |

One `WebKitWebProcess` is shared by every window; the large-page growth is
page content (DOM, highlighting), not extra processes.

## htmlview's recorded shared-window numbers

| Windows | PSS | Private | Open |
|---|---|---|---|
| 1 | 239 MiB | 109 MiB | 0.38 s |
| 2 | 249 MiB | 119 MiB | 0.12 s |
| 3 | 259 MiB | 129 MiB | 0.15 s |

## Warm open, 2,000-line markdown (limpid, 5 runs)

Render+write alone is 0.12 s (35 MB max RSS). Warm open, host already running:
0.155–0.220 s, median ~0.17 s. The design goal is under 0.15 s; limpid is just
over it, and the render, not the host, is the cost.

## Notes

- `fetch()` and ES-module import from `file://` work in limpid and htmlview,
  and are blocked in all three browsers.
- Cold start is ~0.31 s against the 0.3 s goal: at the goal on the small page,
  slightly over on the large one.


## limpid-view (native viewer)

Measured with the release build (`target/release/limpid`), `bench/measure_memory.py`,
`LIMPID_WINDOW_CLASS=limpid-test`, and a Hyprland rule hiding the windows.
Memory is the view process, in MiB.

| Scenario | PSS | anon | file | RSS |
| --- | --- | --- | --- | --- |
| One window, `sample.md`, cold | 57.6 | 29.2 | 16.9 | 71.0 |
| One window, `sample.md`, warm | 57.5 | - | - | - |
| Three windows in one view (sample, small, sample) | 71.8 | 32.0 | - | 96.6 |
| Two windows shared plus one `--separate`, cold (total) | 89.8 | - | - | - |
| Two windows shared plus one `--separate`, warm (total) | 90.3 | - | - | - |

The WU-V1 target was 20 MiB or less for one window. The measured figure is
about three times that; the target is not met. A second window in the same
view costs about 7 MiB PSS, against about 32 MiB for a separate process, so
sharing one view is the cheaper way to hold several windows.

Re-measured by the review pass on 2026-10-26 with `bench/measure_memory.py` (private = Pss_Anon, MiB) and `bench/measure_startup.py`:

| Page | PSS | Private | Window |
|---|---|---|---|
| `# Empty` + one line | 37.0 | 10.0 | |
| `bench/small.md` | 53.6 | 25.8 | |
| `bench/sample.md` | 57.3 | 29.1 | 0.27-0.31 s |
| `small.md` with its code fences removed | 37.7 | 10.7 | |

The content cost is almost entirely the code-highlighting path (tree-sitter
query compilation plus the doubled HTML render inside `doc::parse`), and the
startup time is the parse running before the window is created. Both are the
first items on the fix list in MINIMAL.md.
