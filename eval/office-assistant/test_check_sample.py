#!/usr/bin/env python3
"""Tests for check_sample.py (stdlib unittest). Run: python3 -I test_check_sample.py -v

Each test writes a tiny valid sample in a temp folder, breaks ONE thing, and
expects exactly the matching problem. No real sample files are needed.
"""
import copy
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check_sample.py"
HEADER = "SYNTHETIC — not real people or companies"
TRANSCRIPT = f"{HEADER}\nMeeting: test\n[09:00] Anna: I will send the report on 2026-10-30.\n[09:01] Ben: ok\n"
GOLD = {
    "_synthetic": HEADER,
    "sample_id": "t-01",
    "action_items": [{
        "id": "a1", "what": "send report",
        "owner": {"name": "Anna", "status": "confirmed"},
        "due": {"date": "2026-10-30", "status": "confirmed"},
        "priority": "normal",
        "evidence": [{"line": 1, "quote": "send the report"}],
    }],
    "decisions": [], "notes": [], "not_actions": [],
}


class CheckCase(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="oa-check-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.gold = copy.deepcopy(GOLD)
        self.transcript = TRANSCRIPT

    def run_check(self):
        (self.tmp / "t-01.transcript.txt").write_text(self.transcript, encoding="utf-8")
        (self.tmp / "t-01.gold.json").write_text(json.dumps(self.gold, ensure_ascii=False), encoding="utf-8")
        p = subprocess.run([sys.executable, "-I", str(SCRIPT), "t-01", str(self.tmp)],
                           capture_output=True, text=True, encoding="utf-8")
        return p.returncode, p.stdout

    def test_valid_sample_passes(self):
        code, out = self.run_check()
        self.assertEqual(code, 0, out)
        self.assertIn("OK: no problems found", out)

    def test_missing_header(self):
        self.transcript = self.transcript.replace(HEADER, "hello", 1)
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("line 1 must be exactly", out)

    def test_quote_not_in_line(self):
        self.gold["action_items"][0]["evidence"][0]["quote"] = "send the invoice"
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("is not found exactly in line 1", out)

    def test_evidence_line_out_of_range(self):
        self.gold["action_items"][0]["evidence"][0]["line"] = 9
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("does not exist", out)

    def test_confirmed_due_without_date(self):
        self.gold["action_items"][0]["due"]["date"] = None
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("without a real date", out)

    def test_confirmed_owner_without_name(self):
        self.gold["action_items"][0]["owner"]["name"] = None
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("cannot be 'confirmed'", out)

    def test_duplicate_id(self):
        self.gold["action_items"].append(copy.deepcopy(self.gold["action_items"][0]))
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("duplicate id", out)

    def test_bad_priority(self):
        self.gold["action_items"][0]["priority"] = "high"
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("priority must be one of", out)


if __name__ == "__main__":
    unittest.main()
