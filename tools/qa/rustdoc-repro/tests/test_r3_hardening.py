"""w40 R1–R4 回归负控（只用标准库；只碰本测试自建的私有临时目录；不运行 Cargo）。

对 base 的 artifact_observer.py 这些用例必须因真实行为偏差而 RED（不是缺模块/导入错误）。
运行: python3 -I -B -m unittest discover -s tools/qa/rustdoc-repro/tests -v
"""
import hashlib
import json
import os
import signal
import stat
import subprocess
import sys
import time
import unittest
from pathlib import Path
from unittest import mock

import test_safety as ts

sys.dont_write_bytecode = True
sys.path.insert(0, str(ts.HERE))
import artifact_observer as obs  # noqa: E402

H = ts.H


def sha(b):
    return hashlib.sha256(b).hexdigest()


class Proc(ts.Base):
    def popen(self, *args):
        return subprocess.Popen([sys.executable, "-I", "-B", str(ts.OBS), *map(str, args)], env=ts.env(self.home),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def ready(self, p):
        line = p.stdout.readline()
        self.assertEqual(json.loads(line)["status"], "ready", line + p.stderr.read() if not line else line)

    def wait_exit(self, p, secs):
        end = time.monotonic() + secs
        while time.monotonic() < end:
            if p.poll() is not None:
                return True
            time.sleep(0.02)
        return False

    def finish(self, p, out):
        """只回收本测试自己的 Popen 子进程。"""
        if p.poll() is None:
            try:
                (out / "stop").write_text("")
            except OSError:
                pass
            if not self.wait_exit(p, 5):
                p.send_signal(signal.SIGTERM)
        return p.communicate(timeout=10)

    def events(self, out):
        return [json.loads(x) for x in (out / "events.jsonl").read_text().splitlines()]


class R1ParentReplaced(Proc):
    def _run(self, replace):
        out = self.root / "obs"
        old_sha = sha(self.rlib.read_bytes())
        p = self.popen("--target", self.target, "--out-dir", out, "--artifact", self.rlib,
                       "--interval-ms", "5", "--max-seconds", "30")
        self.ready(p)
        time.sleep(0.05)
        saved = self.deps.with_name("deps.saved")
        os.rename(self.deps, saved)
        if replace:
            self.deps.mkdir()
            self.rlib.write_bytes(b"completely-different-new-dir-content")
        exited = self.wait_exit(p, 5)
        cur = sha(self.rlib.read_bytes()) if self.rlib.exists() else None
        _, err = self.finish(p, out)
        finals = [e for e in self.events(out) if e["kind"] == "final"]
        detail = (f"persistent parent replacement: exited_before_stop={exited} exit={p.returncode} "
                  f"final_sha={[e['new'].get('sha256') for e in finals]} old_dir_sha={old_sha} "
                  f"exact_path_sha_now={cur}")
        self.assertTrue(exited, detail)
        self.assertEqual(p.returncode, 5, detail + err)
        ev = self.events(out)
        self.assertTrue(any(e["kind"] == "path_detached" for e in ev), detail)
        self.assertFalse(any(e["kind"] == "final" and e["new"].get("sha256") == old_sha for e in ev), detail)
        self.assertEqual(json.loads((out / "summary.json").read_text())["stop_reason"], "path_detached")

    def test_parent_dir_renamed_and_replaced(self):
        self._run(True)

    def test_parent_dir_renamed_away(self):
        self._run(False)


class SnapSchedule(ts.Base):
    """在 snap() 内部由测试安排确定性的调度（只是调度负控，不冒称真实并发概率）。"""

    def setUp(self):
        super().setUp()
        tfd = obs.walk_dirs(obs.components(str(self.target), "target"), "target")
        self.art = obs.open_artifacts(str(self.target), tfd, [str(self.rlib)])[0]
        self.old = self.rlib.read_bytes()
        self.old_ino = os.stat(self.rlib).st_ino

    def hooked_snap(self, trigger_on, action):
        real_open, real_read, fired = os.open, os.read, []

        def fake_open(path, flags, *a, **k):
            if trigger_on == "open" and not fired and k.get("dir_fd") is not None and path == self.art["name"]:
                fired.append(1)
                action()
            return real_open(path, flags, *a, **k)

        def fake_read(fd, n):
            if trigger_on == "read" and not fired and stat.S_ISREG(os.fstat(fd).st_mode):
                fired.append(1)
                action()
            return real_read(fd, n)

        with mock.patch.object(os, "open", fake_open), mock.patch.object(os, "read", fake_read):
            try:
                res = ("snap", obs.snap(self.art, True))
            except Exception as e:  # noqa: BLE001  fixed 版以 Detached 表示目录链失效
                res = ("raised", type(e).__name__)
        self.assertTrue(fired, "调度点未触发，负控无效")
        return res

    def replace_atomic(self):
        tmp = self.deps / "tmpnew"
        tmp.write_bytes(b"new-inode-content!!")
        os.replace(tmp, self.rlib)

    def append_in_place(self):
        with open(self.rlib, "r+b") as f:
            f.seek(0, 2)
            f.write(b"-appended-in-place")

    def assert_not_mixed_or_stale(self, res, valid_pairs):
        kind, val = res
        if kind == "raised" or "unstable" in val:
            return
        cur = os.stat(self.rlib)
        pair = (val["ino"], val["size"], val["sha256"])
        self.assertIn(pair, valid_pairs, f"混合快照: {val}")
        self.assertEqual(val["ino"], cur.st_ino, f"稳定快照的 inode 已不是当前路径绑定的 inode: {val} 当前 ino={cur.st_ino}")

    def test_atomic_replace_between_meta_and_open_is_not_mixed(self):
        res = self.hooked_snap("open", self.replace_atomic)
        new = b"new-inode-content!!"
        self.assert_not_mixed_or_stale(res, {(os.stat(self.rlib).st_ino, len(new), sha(new))})

    def test_atomic_replace_after_open_is_unstable_not_stable_old(self):
        res = self.hooked_snap("read", self.replace_atomic)
        self.assert_not_mixed_or_stale(res, set())

    def test_in_place_write_during_hash_is_unstable_not_mixed(self):
        res = self.hooked_snap("read", self.append_in_place)
        new = self.old + b"-appended-in-place"
        self.assert_not_mixed_or_stale(res, {(self.old_ino, len(new), sha(new))})
        self.assertTrue(res[0] == "raised" or "unstable" in res[1], f"原地写入未被标 unstable: {res}")

    def test_parent_swapped_during_snapshot_is_flagged(self):
        def swap():
            os.rename(self.deps, self.deps.with_name("deps.saved"))
            self.deps.mkdir()
            self.rlib.write_bytes(b"other")
        res = self.hooked_snap("open", swap)
        self.assertTrue(res[0] == "raised" or "unstable" in res[1],
                        f"目录链在快照期间被替换，却返回稳定快照: {res}")


class R3ListFile(Proc):
    def listed(self, *extra, name="list.txt", data=None):
        f = self.root / name
        if data is not None:
            f.write_bytes(data)
        return self.popen("--target", self.target, "--out-dir", self.root / "obs", "--artifacts-file", f,
                          "--max-seconds", "0.3", *extra)

    def code(self, p):
        self.assertTrue(self.wait_exit(p, 3), "启动阶段未在 3s 内返回（结构化错误/正常退出均未发生）")
        out, err = p.communicate()
        self.assertEqual(p.returncode, 2, out + err)
        return json.loads(err.strip().splitlines()[-1])

    def test_fifo_artifacts_file_rejected_immediately(self):
        fifo = self.root / "fifo"
        os.mkfifo(fifo, 0o600)
        p = self.popen("--target", self.target, "--out-dir", self.root / "obs", "--artifacts-file", fifo)
        try:
            err = self.code(p)
        finally:
            if p.poll() is None:
                p.send_signal(signal.SIGTERM)   # 仅回收本测试自己的 Popen
                p.communicate(timeout=10)
        self.assertEqual(err["code"], "artifacts_file_not_regular")
        self.assertFalse((self.root / "obs").exists())

    def test_non_utf8_list_rejected(self):
        p = self.listed(data=b"\xff\xfe" + str(self.rlib).encode() + b"\n")
        self.assertEqual(self.code(p)["code"], "artifacts_file_not_utf8")
        self.assertFalse((self.root / "obs").exists())

    def test_oversize_list_rejected(self):
        f = self.root / "big.txt"
        f.write_bytes(b"#" + b"x" * (2 << 20) + b"\n" + str(self.rlib).encode() + b"\n")
        p = self.popen("--target", self.target, "--out-dir", self.root / "obs", "--artifacts-file", f,
                       "--max-seconds", "0.3")
        self.assertEqual(self.code(p)["code"], "artifacts_file_too_large")

    def test_too_many_lines_rejected(self):
        p = self.listed(data=(str(self.rlib) + "\n").encode() * 5000)
        self.assertEqual(self.code(p)["code"], "artifacts_file_too_many_lines")

    def test_valid_list_still_works(self):
        p = self.listed(data=(str(self.rlib) + "\n# c\n" + str(self.so) + "\n").encode())
        self.assertTrue(self.wait_exit(p, 5))
        p.communicate()
        self.assertEqual(p.returncode, 4)

    def test_max_seconds_range(self):
        for bad in ("nan", "inf", "-1", "0", "7200.5"):
            err = self.reject("--target", self.target, "--out-dir", self.root / f"o{bad}",
                              "--artifact", self.rlib, "--max-seconds", bad)
            self.assertEqual(err["code"], "bad_max_seconds", bad)


class R4Names(Proc):
    def test_trailing_newline_and_control_names_rejected(self):
        for i, suffix in enumerate(["\n", "\t", "\x1b", "\r", "\x00x"]):
            name = f"libfx_store-{H}.rlib{suffix}"
            try:
                (self.deps / name).write_bytes(b"x")
            except (OSError, ValueError):
                continue
            p = self.popen("--target", self.target, "--out-dir", self.root / f"o{i}",
                           "--artifact", self.deps / name, "--max-seconds", "0.3")
            self.assertTrue(self.wait_exit(p, 3), f"{suffix!r} 被接受并持续运行")
            out, err = p.communicate()
            self.assertEqual(p.returncode, 2, f"{suffix!r}: {out}{err}")
            self.assertIn(json.loads(err.strip().splitlines()[-1])["code"], {"bad_name", "bad_component"})
            self.assertFalse((self.root / f"o{i}").exists())


if __name__ == "__main__":
    unittest.main()
