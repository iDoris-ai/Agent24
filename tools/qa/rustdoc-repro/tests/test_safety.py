"""安全负控与观察器行为测试（只用标准库；只碰本测试自建的私有临时目录；不运行 Cargo）。

运行: python3 -I -m unittest discover -s tools/qa/rustdoc-repro/tests -v
"""
import json
import os
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
OBS = HERE / "artifact_observer.py"
H = "0123456789abcdef"


def env(home):
    return {"PATH": "/usr/bin:/bin", "HOME": str(home), "TMPDIR": str(home), "LC_ALL": "C"}


class Base(unittest.TestCase):
    def setUp(self):
        self._td = tempfile.TemporaryDirectory(prefix="a24obs-")
        self.root = Path(os.path.realpath(self._td.name))
        os.chmod(self.root, 0o700)
        self.home = self.root / "home"
        self.home.mkdir(mode=0o700)
        self.target = self.root / "target"
        self.deps = self.target / "debug" / "deps"
        self.deps.mkdir(parents=True)
        self.rlib = self.deps / f"libfx_store-{H}.rlib"
        self.rlib.write_bytes(b"v1")
        self.so = self.deps / f"libfake_async_trait-{H}.so"
        self.so.write_bytes(b"pm1")
        self.victim = self.root / "victim"
        self.victim.mkdir(mode=0o700)
        (self.victim / "keep.txt").write_text("victim")
        (self.victim / "stop").write_text("pre-existing stop")

    def tearDown(self):
        self._td.cleanup()

    def victim_intact(self):
        self.assertEqual((self.victim / "keep.txt").read_text(), "victim")
        self.assertEqual((self.victim / "stop").read_text(), "pre-existing stop")
        self.assertEqual(sorted(p.name for p in self.victim.iterdir()), ["keep.txt", "stop"])

    def run_obs(self, *args, timeout=20):
        return subprocess.run([sys.executable, "-I", str(OBS), *map(str, args)],
                              env=env(self.home), capture_output=True, text=True, timeout=timeout)

    def reject(self, *args):
        p = self.run_obs(*args)
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)
        err = json.loads(p.stderr.strip().splitlines()[-1])
        self.assertEqual(err["status"], "error")
        return err


class LegacyFailClosed(Base):
    """F3：历史脚本必须默认拒绝执行，且不得删除/复用/覆写已有路径。"""

    def run_legacy(self, name, *args):
        return subprocess.run(["bash", str(HERE / name), *map(str, args)], env=env(self.home),
                              capture_output=True, text=True, timeout=5)

    def test_run_experiments_refuses_and_keeps_workdir(self):
        p = self.run_legacy("run-experiments.sh", self.victim, "1.95.0")
        self.assertNotEqual(p.returncode, 0)
        self.victim_intact()

    def test_make_fixture_refuses_existing_root(self):
        p = self.run_legacy("make-fixture.sh", self.victim)
        self.assertNotEqual(p.returncode, 0)
        self.victim_intact()

    def test_window_watch_refuses_and_keeps_stop_and_symlink_victim(self):
        out = self.root / "out.log"
        os.symlink(self.victim / "keep.txt", out)
        os.symlink(self.victim / "stop", self.root / "out.log.stop")
        p = self.run_legacy("target-window-watch.sh", self.target, out, "fx_store")
        self.assertNotEqual(p.returncode, 0)
        self.victim_intact()


class ObserverRejects(Base):
    def a(self):
        return ["--artifact", self.rlib]

    def test_existing_out_dir(self):
        self.reject("--target", self.target, "--out-dir", self.victim, *self.a())
        self.victim_intact()

    def test_symlink_out_dir(self):
        link = self.root / "outlink"
        os.symlink(self.victim, link)
        self.reject("--target", self.target, "--out-dir", link, *self.a())
        self.victim_intact()

    def test_symlink_ancestor_of_out(self):
        link = self.root / "anc"
        os.symlink(self.victim, link)
        self.reject("--target", self.target, "--out-dir", link / "new", *self.a())
        self.victim_intact()

    def test_out_inside_target(self):
        self.reject("--target", self.target, "--out-dir", self.target / "obs", *self.a())
        self.assertFalse((self.target / "obs").exists())

    def test_target_inside_out_parent_overlap(self):
        # 输出目录是 target 的祖先子路径之一：target 在输出目录里面
        self.reject("--target", self.target, "--out-dir", self.root, *self.a())

    def test_missing_target(self):
        self.reject("--target", self.root / "nope", "--out-dir", self.root / "o", *self.a())
        self.assertFalse((self.root / "o").exists())

    def test_artifact_symlink_escape(self):
        esc = self.deps / f"libfx_evil-{H}.rlib"
        os.symlink(self.victim / "keep.txt", esc)
        self.reject("--target", self.target, "--out-dir", self.root / "o", "--artifact", esc)
        self.victim_intact()

    def test_target_subdir_symlink_escape(self):
        os.symlink(self.victim, self.target / "debug" / "esc")
        bad = self.target / "debug" / "esc" / f"libfx_store-{H}.rlib"
        self.reject("--target", self.target, "--out-dir", self.root / "o", "--artifact", bad)
        self.victim_intact()

    def test_artifact_outside_target(self):
        self.reject("--target", self.target, "--out-dir", self.root / "o", "--artifact", self.victim / "keep.txt")

    def test_dotdot_artifact(self):
        self.reject("--target", self.target, "--out-dir", self.root / "o",
                    "--artifact", f"{self.deps}/../deps/libfx_store-{H}.rlib")

    def test_illegal_prefix(self):
        bad = self.deps / f"evil;rm-{H}.rlib"
        bad.write_bytes(b"x")
        self.reject("--target", self.target, "--out-dir", self.root / "o", "--artifact", bad)

    def test_out_parent_not_writable(self):
        ro = self.root / "ro"
        ro.mkdir(mode=0o500)
        self.reject("--target", self.target, "--out-dir", ro / "o", *self.a())
        self.assertFalse((ro / "o").exists())

    def test_out_parent_group_writable(self):
        gw = self.root / "gw"
        gw.mkdir()
        os.chmod(gw, 0o770)
        self.reject("--target", self.target, "--out-dir", gw / "o", *self.a())

    def test_no_artifacts(self):
        self.reject("--target", self.target, "--out-dir", self.root / "o")


class ObserverRuns(Base):
    def start(self, *extra):
        out = self.root / "obs"
        p = subprocess.Popen([sys.executable, "-I", str(OBS), "--target", self.target, "--out-dir", out,
                              "--artifact", self.rlib, "--artifact", self.so, "--interval-ms", "5", *extra],
                             env=env(self.home), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        line = p.stdout.readline()
        self.assertEqual(json.loads(line)["status"], "ready", line + p.stderr.read() if not line else line)
        self.assertTrue((out / "ready").exists())
        self.assertEqual(stat.S_IMODE(os.stat(out).st_mode), 0o700)
        return p, out

    def events(self, out):
        return [json.loads(x) for x in (out / "events.jsonl").read_text().splitlines()]

    def test_change_events_and_stop(self):
        p, out = self.start("--max-seconds", "20")
        time.sleep(0.1)
        tmp = self.deps / "tmpnew"
        tmp.write_bytes(b"v2-new-inode")
        os.replace(tmp, self.rlib)            # 新 inode 替换
        time.sleep(0.1)
        with open(self.so, "r+b") as f:       # 原地改内容
            f.write(b"PM")
        time.sleep(0.1)
        (out / "stop").write_text("")
        _, err = p.communicate(timeout=10)
        self.assertEqual(p.returncode, 0, err)
        ev = self.events(out)
        kinds = [e["kind"] for e in ev]
        self.assertEqual(kinds[0], "baseline")
        ch = [e for e in ev if e["kind"] == "change"]
        self.assertTrue(any(e["path"].endswith(".rlib") and e["new"]["sha256"] for e in ch))
        self.assertTrue(any(e["path"].endswith(".so") for e in ch))
        self.assertTrue(all("mono_ns" in e for e in ev))
        summ = json.loads((out / "summary.json").read_text())
        self.assertEqual(summ["stop_reason"], "stop_file")
        self.assertIn("unobserved", summ["limitations"][0])
        self.assertEqual(self.rlib.read_bytes(), b"v2-new-inode")   # 未改动 target 内容

    def test_sigterm_stops_cleanly(self):
        p, out = self.start("--max-seconds", "20")
        p.send_signal(signal.SIGTERM)
        p.communicate(timeout=10)
        self.assertEqual(p.returncode, 0)
        self.assertEqual(json.loads((out / "summary.json").read_text())["stop_reason"], "signal")

    def test_time_limit_exit_4(self):
        p, out = self.start("--max-seconds", "1")
        p.communicate(timeout=10)
        self.assertEqual(p.returncode, 4)
        self.assertEqual(json.loads((out / "summary.json").read_text())["stop_reason"], "max_seconds")

    def test_artifact_vanishes_recorded(self):
        p, out = self.start("--max-seconds", "20")
        hidden = self.deps / "hidden"
        os.rename(self.rlib, hidden)
        time.sleep(0.1)
        os.rename(hidden, self.rlib)
        time.sleep(0.1)
        (out / "stop").write_text("")
        p.communicate(timeout=10)
        self.assertEqual(p.returncode, 0)
        self.assertTrue(any(e["kind"] == "change" and e["new"]["exists"] is False for e in self.events(out)))
        self.assertTrue(self.rlib.exists())


if __name__ == "__main__":
    unittest.main()
