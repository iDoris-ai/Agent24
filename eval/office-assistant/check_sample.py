#!/usr/bin/env python3
"""Check one OA-M1 sample: <id>.transcript.txt + <id>.gold.json (format: FORMAT.md).

Usage: python3 -I check_sample.py th-01 [samples_dir]   (default dir: samples/ next to this file)
Exit code 0 = no problems, 1 = problems found.
"""
import json
import re
import sys
from pathlib import Path

HEADER = "SYNTHETIC — not real people or companies"
UTTERANCE = re.compile(r"^\[(\d{2}):(\d{2})\] ([^:]+): (.*)$")
OWNER_STATUS = {"confirmed", "needs_confirmation"}
DUE_STATUS = {"confirmed", "needs_confirmation"}
PRIORITY = {"normal", "urgent"}
NOT_ACTION_REASON = {"instruction_in_material", "question_not_agreement", "joke"}
ISO_DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def load_lines(path):
    """Return the utterance texts; line 1 is the first [HH:MM] line."""
    raw = path.read_text(encoding="utf-8").splitlines()
    problems = []
    if not raw or raw[0] != HEADER:
        problems.append(f"{path.name}: line 1 must be exactly '{HEADER}'")
    if len(raw) < 2 or not raw[1].startswith("Meeting: "):
        problems.append(f"{path.name}: line 2 must start with 'Meeting: '")
    texts = []
    for n, line in enumerate(raw[2:], start=3):
        if not line.strip():
            continue
        m = UTTERANCE.match(line)
        if not m:
            problems.append(f"{path.name}: file line {n} is not '[HH:MM] Speaker: text'")
            continue
        texts.append(m.group(4))
    return texts, problems


def check_evidence(where, evidence, texts):
    problems = []
    if not evidence:
        problems.append(f"{where}: needs at least one evidence item")
    for ev in evidence or []:
        line, quote = ev.get("line"), ev.get("quote")
        if not isinstance(line, int) or not 1 <= line <= len(texts):
            problems.append(f"{where}: line {line!r} does not exist (1..{len(texts)})")
        elif not quote or quote not in texts[line - 1]:
            problems.append(f"{where}: quote {quote!r} is not found exactly in line {line}")
    return problems


def check(sample_id, folder):
    transcript = folder / f"{sample_id}.transcript.txt"
    gold_path = folder / f"{sample_id}.gold.json"
    texts, problems = load_lines(transcript)
    gold = json.loads(gold_path.read_text(encoding="utf-8"))

    if gold.get("_synthetic") != HEADER:
        problems.append("gold: '_synthetic' must be the SYNTHETIC header")
    if gold.get("sample_id") != sample_id:
        problems.append(f"gold: sample_id is {gold.get('sample_id')!r}, expected {sample_id!r}")

    seen = set()
    for item in gold.get("action_items", []):
        where = f"action {item.get('id')}"
        if item.get("id") in seen:
            problems.append(f"{where}: duplicate id")
        seen.add(item.get("id"))
        owner, due = item.get("owner", {}), item.get("due", {})
        if owner.get("status") not in OWNER_STATUS:
            problems.append(f"{where}: owner.status must be one of {sorted(OWNER_STATUS)}")
        if owner.get("name") is None and owner.get("status") == "confirmed":
            problems.append(f"{where}: an owner with no name cannot be 'confirmed'")
        if due.get("status") not in DUE_STATUS:
            problems.append(f"{where}: due.status must be one of {sorted(DUE_STATUS)}")
        if due.get("date") is not None and not ISO_DATE.match(due["date"]):
            problems.append(f"{where}: due.date must be YYYY-MM-DD or null")
        if due.get("date") is None and due.get("status") == "confirmed":
            problems.append(f"{where}: due cannot be 'confirmed' without a real date")
        if item.get("priority") not in PRIORITY:
            problems.append(f"{where}: priority must be one of {sorted(PRIORITY)}")
        problems += check_evidence(where, item.get("evidence"), texts)

    for kind in ("decisions", "notes"):
        for entry in gold.get(kind, []):
            problems += check_evidence(f"{kind[:-1]} {entry.get('id')}", entry.get("evidence"), texts)

    for na in gold.get("not_actions", []):
        where = f"not_action line {na.get('line')}"
        if na.get("reason") not in NOT_ACTION_REASON:
            problems.append(f"{where}: reason must be one of {sorted(NOT_ACTION_REASON)}")
        problems += check_evidence(where, [na], texts)

    return texts, gold, problems


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__.strip())
        return 2
    folder = Path(sys.argv[2]) if len(sys.argv) == 3 else Path(__file__).resolve().parent / "samples"
    texts, gold, problems = check(sys.argv[1], folder)
    print(f"{sys.argv[1]}: {len(texts)} lines, {len(gold.get('action_items', []))} action items, "
          f"{len(gold.get('decisions', []))} decisions, {len(gold.get('notes', []))} notes, "
          f"{len(gold.get('not_actions', []))} not-actions")
    for p in problems:
        print("  PROBLEM:", p)
    print("  OK: no problems found" if not problems else f"  {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
