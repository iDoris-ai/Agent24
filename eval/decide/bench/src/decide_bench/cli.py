"""``uv run bench`` — the one command PLAN-DECIDE D0-5 asks for to run the
whole horizontal evaluation.

Examples
--------
    uv run bench --all --machine m1max-64g
    uv run bench --all --candidates rule,gliclass-multilang --sets retain_intent

Each candidate runs in its own subprocess (``--_run_one``, an internal flag
this module re-invokes itself with — not part of the public CLI). Two
reasons: (1) peak RSS is only meaningful per-candidate if candidates don't
share a process accumulating memory from models loaded earlier in the
run; (2) a candidate that genuinely crashes the interpreter (not just
raises ``CandidateUnavailable``) must not take the rest of the run down
with it — "单个候选失败不终止全局" has to hold even for a segfault, not
just a clean exception.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from datetime import date, datetime, timezone
from pathlib import Path

from .adapters import build_registry
from .data import load_sets
from .hardware import default_machine_id, machine_info
from .report import render_markdown
from .runner import result_to_dict, run_candidate
from .types import Point

RESULTS_DIR = Path(__file__).resolve().parents[3] / "results"


def _run_one_subprocess(name: str, points: list[Point], timeout_s: float = 1800.0) -> dict:
    with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as tmp:
        out_path = Path(tmp.name)
    try:
        proc = subprocess.run(
            [
                sys.executable,
                "-m",
                "decide_bench.cli",
                "--_run_one",
                name,
                "--sets",
                ",".join(points),
                "--_out",
                str(out_path),
            ],
            capture_output=True,
            text=True,
            timeout=timeout_s,
        )
        if proc.returncode != 0 or not out_path.exists():
            tail = (proc.stderr or "")[-4000:]
            return {
                "name": name,
                "model_id": None,
                "revision": None,
                "resolved_sha": None,
                "available": False,
                "unavailable_reason": f"subprocess exited {proc.returncode}: {tail}",
                "load_latency_s": None,
                "peak_rss_mb": None,
                "download_size_mb": None,
                "points": {},
            }
        return json.loads(out_path.read_text(encoding="utf-8"))
    except subprocess.TimeoutExpired:
        return {
            "name": name,
            "model_id": None,
            "revision": None,
            "resolved_sha": None,
            "available": False,
            "unavailable_reason": f"subprocess timed out after {timeout_s}s",
            "load_latency_s": None,
            "peak_rss_mb": None,
            "download_size_mb": None,
            "points": {},
        }
    finally:
        out_path.unlink(missing_ok=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="bench", description=__doc__)
    parser.add_argument("--all", action="store_true", help="run every registered candidate")
    parser.add_argument("--candidates", type=str, default=None, help="comma-separated candidate names (default: all)")
    parser.add_argument("--sets", type=str, default=None, help="comma-separated point names (default: all three)")
    parser.add_argument("--machine", type=str, default=None, help="machine-id for results/<machine-id>/ (e.g. m1max-64g)")
    parser.add_argument("--out-dir", type=str, default=None, help="override results output directory")
    parser.add_argument("--_run_one", type=str, default=None, help=argparse.SUPPRESS)
    parser.add_argument("--_out", type=str, default=None, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)

    points: list[Point] = (
        [p.strip() for p in args.sets.split(",")] if args.sets else ["retain_intent", "recall_gate", "tool_risk"]
    )

    if args._run_one:
        # Internal worker mode: run exactly one candidate in this process
        # and write its CandidateResult JSON to --_out. Invoked by the
        # orchestrating process below, never by a human.
        registry = build_registry()
        if args._run_one not in registry:
            print(f"unknown candidate: {args._run_one}", file=sys.stderr)
            return 2
        items_by_point = load_sets(points)
        result = run_candidate(args._run_one, registry[args._run_one], items_by_point, points)
        Path(args._out).write_text(json.dumps(result_to_dict(result), ensure_ascii=False), encoding="utf-8")
        return 0

    registry = build_registry()
    if args.candidates:
        names = [n.strip() for n in args.candidates.split(",") if n.strip()]
        unknown = [n for n in names if n not in registry]
        if unknown:
            print(f"unknown candidate(s): {unknown}; available: {sorted(registry)}", file=sys.stderr)
            return 2
    else:
        names = sorted(registry)
        if not args.all and not args.candidates:
            print("note: no --candidates given and --all not set; running all registered candidates anyway", file=sys.stderr)

    machine_id = args.machine or default_machine_id()

    candidate_results = []
    for name in names:
        print(f"=== running {name} ===", file=sys.stderr)
        result_dict = _run_one_subprocess(name, points)
        if not result_dict["available"]:
            print(f"[{name}] UNAVAILABLE: {result_dict['unavailable_reason']}", file=sys.stderr)
        candidate_results.append(result_dict)

    run = {
        "schema_version": 1,
        "date": date.today().isoformat(),
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "machine_id": machine_id,
        "machine_info": machine_info(),
        "points_run": points,
        "candidates": candidate_results,
    }

    out_dir = Path(args.out_dir) if args.out_dir else RESULTS_DIR / machine_id
    out_dir.mkdir(parents=True, exist_ok=True)
    today = date.today().isoformat()
    json_path = out_dir / f"{today}.json"
    md_path = out_dir / f"{today}.md"
    json_path.write_text(json.dumps(run, ensure_ascii=False, indent=2), encoding="utf-8")
    md_path.write_text(render_markdown(run), encoding="utf-8")
    print(f"wrote {json_path}", file=sys.stderr)
    print(f"wrote {md_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
