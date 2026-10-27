#!/usr/bin/env -S python3 -I
"""Launch a command under a private XDG_RUNTIME_DIR, wait, dump per-process
PSS from smaps_rollup for every process carrying that marker, then kill them.

usage: measure.py <label> <settle_seconds> -- <cmd...>
"""
import os, signal, subprocess, sys, tempfile, time
from pathlib import Path

label = sys.argv[1]
settle = float(sys.argv[2])
assert sys.argv[3] == "--"
cmd = sys.argv[4:]

runtime = tempfile.mkdtemp(prefix="limpid-measure-")
os.chmod(runtime, 0o700)
real_runtime = os.environ.get("XDG_RUNTIME_DIR", "")
wl = os.environ.get("WAYLAND_DISPLAY", "wayland-1")
for name in (wl, wl + ".lock"):
    src = Path(real_runtime) / name
    if src.exists():
        os.symlink(src, Path(runtime) / name)
env = dict(os.environ, XDG_RUNTIME_DIR=runtime, LIMPID_WINDOW_CLASS="limpid-test")
for k, v in [kv.split("=", 1) for kv in os.environ.get("MEASURE_ENV", "").split() if "=" in kv]:
    env[k] = v
marker = f"XDG_RUNTIME_DIR={runtime}".encode()

t0 = time.monotonic()
proc = subprocess.Popen(cmd, env=env, stdin=subprocess.DEVNULL)
time.sleep(settle)

def kb(path, key):
    try:
        for line in Path(path).read_text().splitlines():
            if line.startswith(key + ":"):
                return int(line.split()[1])
    except OSError:
        pass
    return 0

rows = []
for p in Path("/proc").iterdir():
    if not p.name.isdigit():
        continue
    try:
        if marker not in (p / "environ").read_bytes():
            continue
        comm = (p / "comm").read_text().strip()
    except OSError:
        continue
    r = p / "smaps_rollup"
    rows.append((comm, int(p.name), kb(r, "Pss"), kb(r, "Pss_Anon"), kb(r, "Pss_File"), kb(r, "Rss")))

print(f"== {label}")
print(f"{'comm':20} {'pid':>7} {'PSS':>8} {'anon':>8} {'file':>8} {'RSS':>8}  (MiB)")
tot = [0, 0, 0, 0]
for comm, pid, pss, anon, file, rss in sorted(rows, key=lambda r: -r[2]):
    print(f"{comm:20} {pid:>7} {pss/1024:8.1f} {anon/1024:8.1f} {file/1024:8.1f} {rss/1024:8.1f}")
    tot[0] += pss; tot[1] += anon; tot[2] += file; tot[3] += rss
print(f"{'TOTAL':20} {'':>7} {tot[0]/1024:8.1f} {tot[1]/1024:8.1f} {tot[2]/1024:8.1f} {tot[3]/1024:8.1f}")

for comm, pid, *_ in rows:
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
time.sleep(0.5)
for comm, pid, *_ in rows:
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
try:
    proc.wait(timeout=2)
except subprocess.TimeoutExpired:
    proc.kill()
import shutil
shutil.rmtree(runtime, ignore_errors=True)
