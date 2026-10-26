#!/usr/bin/env -S python3 -I
"""Time from spawn until Hyprland lists a window whose title contains TITLE.
usage: startup.py <title-substring> <runs> -- <cmd...>"""
import json, subprocess, sys, time

title = sys.argv[1]
runs = int(sys.argv[2])
assert sys.argv[3] == "--"
cmd = sys.argv[4:]

def windows():
    out = subprocess.run(["hyprctl", "clients", "-j"], capture_output=True, text=True).stdout
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return []

times = []
for _ in range(runs):
    before = {w["address"] for w in windows()}
    t0 = time.monotonic()
    proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    found = None
    while time.monotonic() - t0 < 10:
        for w in windows():
            if w["address"] not in before and title in w.get("title", ""):
                found = time.monotonic() - t0
                break
        if found is not None:
            break
        time.sleep(0.005)
    times.append(found)
    proc.terminate()
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        proc.kill()
    time.sleep(0.3)
print(title, ["%.3f" % t if t else None for t in times])
