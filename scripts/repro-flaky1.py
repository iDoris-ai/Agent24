#!/usr/bin/env python3
"""Repeat built Rust test binaries under CPU pressure; keep every result."""

import argparse
import concurrent.futures
import ctypes
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import time


def cpu_ticks():
    if os.uname().sysname != "Darwin":
        return None
    system = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    ticks = (ctypes.c_uint32 * 4)()
    size = ctypes.c_uint32(4)
    if system.host_statistics(system.mach_host_self(), 3, ctypes.byref(ticks), ctypes.byref(size)):
        return None
    # HOST_CPU_LOAD_INFO: user, system, idle, nice.
    return sum(ticks), ticks[2]


def daemon_pids(binary):
    if os.uname().sysname != "Darwin":
        return set()
    system = ctypes.CDLL("/usr/lib/libproc.dylib")
    count = system.proc_listallpids(None, 0)
    pids = (ctypes.c_int * (count + 512))()
    found = system.proc_listallpids(pids, ctypes.sizeof(pids))
    owned = set()
    for pid in pids[:found]:
        path = ctypes.create_string_buffer(4096)
        if system.proc_pidpath(pid, path, len(path)) > 0 and os.fsdecode(path.value) == str(binary):
            owned.add(pid)
    return owned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", type=Path, required=True)
    parser.add_argument("--logs", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=50)
    parser.add_argument("--parallel", type=int, default=4)
    parser.add_argument("--burners", type=int, default=2 * (os.cpu_count() or 1))
    parser.add_argument("--suite", choices=["card", "attach", "wal", "all"], default="all")
    args = parser.parse_args()
    if args.runs < 1 or args.parallel < 1 or args.burners < 0:
        parser.error("runs and parallel must be positive; burners must be nonnegative")
    resource.setrlimit(resource.RLIMIT_NOFILE, (4096, resource.getrlimit(resource.RLIMIT_NOFILE)[1]))
    args.logs.mkdir(parents=True, exist_ok=True)
    suites = {
        "card": ("module_tool_card", []),
        "attach": ("attach_cli", []),
        "wal": ("agent24_memory", ["wal_maintenance::tests::"]),
    }
    commands = {}
    for suite, (name, filters) in suites.items():
        if args.suite not in (suite, "all"):
            continue
        binaries = [p for p in (args.target / "debug/deps").glob(name + "-*")
                    if p.is_file() and "." not in p.name and os.access(p, os.X_OK)]
        if len(binaries) != 1:
            raise RuntimeError(f"expected one {name} binary, found {binaries}")
        commands[suite] = [str(binaries[0].resolve()), *filters, "--nocapture"]
    metadata = {"commands": commands, "runs": args.runs, "parallel": args.parallel,
                "burners": args.burners, "cores": os.cpu_count(), "load_before": os.getloadavg()}
    executables = [Path(cmd[0]) for cmd in commands.values()]
    executables += [args.target / "debug" / name for name in ("agent24d", "agent24")]
    metadata["sha256"] = {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                          for p in executables if p.is_file()}
    (args.logs / "config.json").write_text(json.dumps(metadata, indent=2) + "\n")
    burners = []
    results = []
    daemons_before = daemon_pids((args.target / "debug/agent24d").resolve())

    def run(suite, iteration):
        started = time.monotonic()
        with (args.logs / f"{suite}-{iteration:02}.log").open("w") as output:
            try:
                child = subprocess.Popen(commands[suite], stdout=output, stderr=subprocess.STDOUT,
                                         start_new_session=True)
                code = child.wait(timeout=180)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
                code = 124
        return {"suite": suite, "iteration": iteration, "code": code,
                "seconds": round(time.monotonic() - started, 3), "load": os.getloadavg()}

    usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    ticks_before = cpu_ticks()
    pressure_started = time.monotonic()
    try:
        for _ in range(args.burners):
            burners.append(subprocess.Popen(["/usr/bin/yes"], stdout=subprocess.DEVNULL))
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.parallel) as executor:
            futures = [executor.submit(run, suite, iteration)
                       for iteration in range(1, args.runs + 1) for suite in commands]
            with (args.logs / "results.jsonl").open("w") as output:
                for future in concurrent.futures.as_completed(futures):
                    result = future.result()
                    results.append(result)
                    output.write(json.dumps(result) + "\n")
                    output.flush()
                    print(json.dumps(result), flush=True)
    finally:
        for burner in burners:
            burner.terminate()
        cpu_seconds = []
        for burner in burners:
            _, _, usage = os.wait4(burner.pid, 0)
            burner.returncode = 0
            cpu_seconds.append(usage.ru_utime + usage.ru_stime)
        pressure_seconds = time.monotonic() - pressure_started
        usage_after = resource.getrusage(resource.RUSAGE_CHILDREN)
        child_cpu = (usage_after.ru_utime + usage_after.ru_stime
                     - usage_before.ru_utime - usage_before.ru_stime)
        pressure = {"cpu_seconds": cpu_seconds, "wall_seconds": pressure_seconds,
                    "burner_cpu_fraction": sum(cpu_seconds) / pressure_seconds / (os.cpu_count() or 1),
                    "reaped_child_cpu_fraction": child_cpu / pressure_seconds / (os.cpu_count() or 1)}
        ticks_after = cpu_ticks()
        if ticks_before and ticks_after and ticks_after[0] > ticks_before[0]:
            pressure["system_cpu_busy_fraction"] = 1 - ((ticks_after[1] - ticks_before[1])
                                                       / (ticks_after[0] - ticks_before[0]))
        (args.logs / "burners.json").write_text(json.dumps(pressure, indent=2) + "\n")
    summary = {suite: {"runs": sum(r["suite"] == suite for r in results),
                       "failed": sum(r["suite"] == suite and r["code"] != 0 for r in results)}
               for suite in commands}
    (args.logs / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    leftovers = sorted(daemon_pids((args.target / "debug/agent24d").resolve()) - daemons_before)
    (args.logs / "leftover-daemons.json").write_text(json.dumps(leftovers) + "\n")
    print(json.dumps(summary), flush=True)
    return int(bool(leftovers) or any(r["code"] != 0 for r in results))


if __name__ == "__main__":
    raise SystemExit(main())
