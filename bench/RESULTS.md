# Benchmark results

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
