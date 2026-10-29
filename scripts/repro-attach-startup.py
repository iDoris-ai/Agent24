#!/usr/bin/env python3
"""Trace CLI startup under CPU pressure while owning every real daemon."""

import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import resource
import runpy
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", type=Path, required=True)
    parser.add_argument("--logs", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=50)
    parser.add_argument("--parallel", type=int, default=4)
    parser.add_argument("--burners", type=int, default=20)
    args = parser.parse_args()
    if args.runs < 1 or args.parallel < 1 or args.burners < 0:
        parser.error("runs/parallel must be positive; burners must be nonnegative")
    resource.setrlimit(resource.RLIMIT_NOFILE, (4096, resource.getrlimit(resource.RLIMIT_NOFILE)[1]))
    args.target = args.target.resolve()
    args.logs = args.logs.resolve()
    args.logs.mkdir(parents=True, exist_ok=True)
    helpers = runpy.run_path(str(Path(__file__).with_name("repro-flaky1.py")))
    daemon = args.target / "debug/agent24d"
    for binary in (daemon, args.target / "debug/agent24"):
        if not binary.is_file():
            parser.error(f"build {binary} first")
    before = helpers["daemon_pids"](daemon)
    # The CLI may kill this relay at any point. It owns no daemon: the broker
    # below spawns and reaps them, including failed and ephemeral starts.
    relay = args.logs / "relay.py"
    relay.write_text("""#!/usr/bin/python3
import json, os, socket, sys
with socket.socket(socket.AF_UNIX) as peer:
    peer.connect(os.environ['STARTUP_BROKER'])
    peer.sendall((json.dumps(sys.argv[1:])+'\\n').encode())
    while True:
        data = peer.recv(65536)
        if not data: break
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()
""")
    relay.chmod(0o755)

    def run(iteration):
        root = args.logs / f"run-{iteration:03}"
        root.mkdir()
        children, threads, errors = [], [], []
        lock = threading.Lock()
        start = time.monotonic()

        def event(name, **fields):
            with lock, (root / "timeline.jsonl").open("a") as output:
                output.write(json.dumps(dict(event=name, seconds=time.monotonic() - start,
                                             **fields)) + "\n")

        def serve(peer, launched):
            try:
                relay_daemon(peer, launched)
            except Exception as error:
                errors.append(str(error))
                event("broker_error", error=str(error))
                peer.close()
            finally:
                launched.set()

        def relay_daemon(peer, launched):
            with peer, peer.makefile("rb") as stream:
                command = json.loads(stream.readline())
                index = len(children)
                with (root / f"daemon-{index}.log").open("w") as stderr:
                    event("spawn", args=command)
                    child = subprocess.Popen([str(daemon), *command], stdin=subprocess.DEVNULL,
                                             stdout=subprocess.PIPE, stderr=stderr,
                                             env=dict(PATH=os.environ.get("PATH", ""),
                                                      HOME=str(root), RUST_LOG="agent24d=debug"))
                    children.append(child)
                    launched.set()
                    event("spawned", pid=child.pid)
                    for line in child.stdout:
                        value = json.loads(line)
                        event("ready", pid=child.pid, port=value["port"])
                        # Forward immediately; health probing must not delay the CLI.
                        try:
                            peer.sendall(line)
                        except BrokenPipeError:
                            break
                        try:
                            with urllib.request.urlopen(
                                f"http://127.0.0.1:{value['port']}/api/v1/health", timeout=10
                            ) as response:
                                event("health", pid=child.pid, status=response.status)
                        except OSError as error:
                            event("health_error", pid=child.pid, error=str(error))

        with tempfile.TemporaryDirectory(prefix="a24trace", dir="/tmp") as sockets:
            with socket.socket(socket.AF_UNIX) as listener:
                address = str(Path(sockets) / "broker.sock")
                listener.bind(address)
                listener.listen()
                listener.settimeout(0.1)
                done = threading.Event()

                def accept():
                    while not done.is_set():
                        try:
                            peer, _ = listener.accept()
                        except TimeoutError:
                            continue
                        launched = threading.Event()
                        thread = threading.Thread(target=serve, args=(peer, launched))
                        threads.append(thread)
                        thread.start()
                        launched.wait()

                broker = threading.Thread(target=accept)
                broker.start()
                results = []
                try:
                    env = dict(PATH=os.environ.get("PATH", ""), HOME=str(root),
                               AGENT24D_BIN=str(relay), STARTUP_BROKER=address)
                    for command in (["daemon", "start"], ["os", "attach", "list", "--json"]):
                        event("cli_start", args=command)
                        try:
                            output = subprocess.run([str(args.target / "debug/agent24"), *command],
                                                    env=env, stdin=subprocess.DEVNULL,
                                                    capture_output=True, timeout=45)
                            result = dict(args=command, code=output.returncode,
                                          stdout=output.stdout.decode(), stderr=output.stderr.decode())
                        except subprocess.TimeoutExpired:
                            result = dict(args=command, code=124)
                        event("cli_end", **result)
                        results.append(result)
                finally:
                    done.set()
                    broker.join()
                    for child in children:
                        child.kill()
                        child.wait()
                        event("reaped", pid=child.pid)
                    for thread in threads:
                        thread.join()
        (root / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        return dict(iteration=iteration, failed=bool(errors) or any(r["code"] for r in results))

    burners = []
    ticks = helpers["cpu_ticks"]()
    try:
        for _ in range(args.burners):
            burners.append(subprocess.Popen(["/usr/bin/yes"], stdout=subprocess.DEVNULL))
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.parallel) as executor:
            results = list(executor.map(run, range(args.runs)))
    finally:
        for child in burners:
            child.terminate()
        for child in burners:
            child.wait()
    after = helpers["cpu_ticks"]()
    leftovers = sorted(helpers["daemon_pids"](daemon) - before)
    summary = dict(runs=len(results), failed=sum(r["failed"] for r in results),
                   burners=args.burners, parallel=args.parallel, leftover_daemons=leftovers)
    if ticks and after:
        summary["system_cpu_busy_fraction"] = 1 - (after[1] - ticks[1]) / (after[0] - ticks[0])
    (args.logs / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary), flush=True)
    return int(bool(leftovers) or bool(summary["failed"]))


if __name__ == "__main__":
    raise SystemExit(main())
