#!/usr/bin/env python3
"""只读 artifact 观察器（w38，仅标准库）：不运行 Cargo、不杀进程、不删改任何文件；只采样调用方给出的精确
artifact 路径的 lstat 纳秒元数据，基线/变化/结束时 sha256；输出只写新建 0700 私有目录（O_EXCL/O_NOFOLLOW/dirfd）。
短瞬态可能漏检，"未观测到变化"≠"没有变化"，变化≠写者身份或因果。用法、退出码见 README.md。
"""
import argparse
import hashlib
import json
import os
import re
import signal
import stat
import sys
import time

NAME_RE = re.compile(r"(lib[A-Za-z0-9_]+-[0-9a-f]{16}\.(rlib|rmeta|so|dylib)|[A-Za-z0-9_]+-[0-9a-f]{16}\.d)")  # 用 fullmatch
LIST_MAX_BYTES, LIST_MAX_LINES = 1 << 20, 4096
DIR_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | getattr(os, "O_CLOEXEC", 0)
LIMITATIONS = [
    "unobserved is not unchanged: 采样间隔内的短瞬态可能漏检，未观测到变化不能排除写者",
    "元数据不变的内容改动仅在 --hash-every-sample 时可能被发现",
    "观察到的变化不说明写者身份、进程或因果",
    "只覆盖调用方给出的精确路径，不代表 target 其他文件",
    "目录链/文件快照校验只能发现采样时刻的替换；两次校验之间来回替换的瞬态仍可能漏检，不证明没有写者",
]


class Detached(Exception):
    """路径的目录链已与启动时绑定的 dev/inode 不一致（被移走、替换或变成符号链接）。"""


class Fail(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code, self.message = code, message


def components(path, what):
    if not os.path.isabs(path):
        raise Fail("not_absolute", f"{what} 必须是绝对路径: {path}")
    parts = path.split("/")[1:]
    if path.endswith("/"):
        parts = parts[:-1]
    for p in parts:
        if p in ("", ".", "..") or any(ord(c) < 32 or ord(c) == 127 for c in p):
            raise Fail("bad_component", f"{what} 含空/./../控制字符组件: {path!r}")
    return parts


def walk_dirs(parts, what, start_fd=None, watch=None, ids=None):
    """逐级 openat(O_NOFOLLOW|O_DIRECTORY)。任一组件是符号链接或不是目录即失败；ids 收集每级 (dev, ino)。"""
    fd = os.open("/", DIR_FLAGS) if start_fd is None else os.dup(start_fd)
    try:
        for p in parts:
            if watch is not None:
                st = os.fstat(fd)
                if (st.st_dev, st.st_ino) == watch:
                    raise Fail("overlap", f"{what} 位于 target 内部")
            try:
                nfd = os.open(p, DIR_FLAGS, dir_fd=fd)
            except FileNotFoundError:
                raise Fail("missing", f"{what} 不存在: {p}")
            except OSError as e:
                raise Fail("symlink_or_not_dir", f"{what} 组件 {p!r} 不是普通目录或是符号链接: {e.strerror}")
            os.close(fd)
            fd = nfd
            if ids is not None:
                ids.append((os.fstat(fd).st_dev, os.fstat(fd).st_ino))
        if watch is not None and (os.fstat(fd).st_dev, os.fstat(fd).st_ino) == watch:
            raise Fail("overlap", f"{what} 位于 target 内部")
        return fd
    except BaseException:
        os.close(fd)
        raise


def check_owner(fd, what, need_private_write=False):
    st = os.fstat(fd)
    if st.st_uid != os.geteuid():
        raise Fail("owner", f"{what} 不属于当前用户")
    if need_private_write:
        if not st.st_mode & stat.S_IWUSR:
            raise Fail("not_writable", f"{what} 对属主不可写")
        if st.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
            raise Fail("shared_writable", f"{what} 对组/其他用户可写")
    return st


def open_artifacts(target, tfd, paths):
    arts, tparts = [], components(target, "target")
    for a in paths:
        parts = components(a, "artifact")
        if parts[: len(tparts)] != tparts or len(parts) <= len(tparts):
            raise Fail("outside_target", f"artifact 不在 target 内: {a}")
        rel, name = parts[len(tparts):-1], parts[-1]
        if not NAME_RE.fullmatch(name):
            raise Fail("bad_name", f"artifact 文件名不是 cargo 产物格式（lib<crate>-<16hex>.<ext> 或 <crate>-<16hex>.d）: {name!r}")
        ids = []
        pfd = walk_dirs(tparts + rel, "artifact 目录", ids=ids)
        tst = os.fstat(tfd)
        if tparts and ids[len(tparts) - 1] != (tst.st_dev, tst.st_ino):
            os.close(pfd)
            raise Fail("target_changed", "启动期间 target 目录被替换")
        try:
            st = os.stat(name, dir_fd=pfd, follow_symlinks=False)
        except FileNotFoundError:
            os.close(pfd)
            raise Fail("missing", f"artifact 在启动时不存在: {a}")
        if not stat.S_ISREG(st.st_mode):
            os.close(pfd)
            raise Fail("not_regular", f"artifact 不是普通文件（可能是符号链接）: {a}")
        os.close(pfd)
        arts.append({"path": a, "name": name, "parts": tparts + rel, "ids": ids})
    if not arts:
        raise Fail("no_artifacts", "至少需要一个 --artifact 或 --artifacts-file")
    if len({x["path"] for x in arts}) != len(arts):
        raise Fail("duplicate", "artifact 路径重复")
    return arts


def chain_fd(art):
    """从 / 起 O_NOFOLLOW 重走整条目录链，dev/inode 必须与启动时一致；返回新的父目录 fd。"""
    ids = []
    try:
        fd = walk_dirs(art["parts"], "artifact 目录链", ids=ids)
    except Fail as e:
        raise Detached(f"{e.code}: {e.message}")
    if ids != art["ids"]:
        os.close(fd)
        raise Detached("目录链 dev/inode 与启动时绑定不一致")
    return fd


def fmeta(st):
    return {"exists": True, "dev": st.st_dev, "ino": st.st_ino, "mode": st.st_mode, "nlink": st.st_nlink,
            "size": st.st_size, "mtime_ns": st.st_mtime_ns, "ctime_ns": st.st_ctime_ns}


def read_fd(fd, name, pfd, with_hash):
    """同一 regular fd：fstat-before → hash → fstat-after，再确认路径仍绑定该 fd；任何变化只标 unstable。"""
    try:
        st1 = os.fstat(fd)
        if not stat.S_ISREG(st1.st_mode):
            return {"exists": True, "unstable": "not_regular"}
        m, h = fmeta(st1), hashlib.sha256()
        while with_hash and (chunk := os.read(fd, 1 << 20)):
            h.update(chunk)
        if fmeta(os.fstat(fd)) != m:
            return {"exists": True, "unstable": "fd_changed_during_snapshot"}
        try:
            pst = os.stat(name, dir_fd=pfd, follow_symlinks=False)
        except FileNotFoundError:
            return {"exists": True, "unstable": "path_no_longer_bound"}
        if (pst.st_dev, pst.st_ino) != (m["dev"], m["ino"]):
            return {"exists": True, "unstable": "path_rebound"}
        if with_hash:
            m["sha256"] = h.hexdigest()
        return m
    finally:
        os.close(fd)


def snap(art, with_hash):
    """目录链校验 → 同一 fd 的 meta+hash → 目录链再校验。目录链失效抛 Detached；文件级变化返回 unstable。"""
    pfd = chain_fd(art)
    try:
        try:
            fd = os.open(art["name"], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | getattr(os, "O_CLOEXEC", 0),
                         dir_fd=pfd)
        except FileNotFoundError:
            m = {"exists": False}
        except OSError as e:
            m = {"exists": True, "unstable": f"open_errno:{e.errno}"}
        else:
            m = read_fd(fd, art["name"], pfd, with_hash)
    finally:
        os.close(pfd)
    os.close(chain_fd(art))
    return m


def stable_snap(art, with_hash, tries=3):
    for _ in range(tries):
        m = snap(art, with_hash)
        if "unstable" not in m:
            break
    return m


def comparable(m):
    return {k: v for k, v in m.items() if k != "sha256"}


class Out:
    def __init__(self, dfd):
        self.dfd = dfd
        self.ev = self.create("events.jsonl")

    def create(self, name):
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | getattr(os, "O_CLOEXEC", 0)
        return os.open(name, flags, 0o600, dir_fd=self.dfd)

    def write_json(self, name, obj):
        fd = self.create(name)
        try:
            os.write(fd, (json.dumps(obj, ensure_ascii=False, indent=2) + "\n").encode())
        finally:
            os.close(fd)

    def event(self, obj):
        obj = {"mono_ns": time.monotonic_ns(), "wall_ns": time.time_ns(), **obj}
        os.write(self.ev, (json.dumps(obj, ensure_ascii=False) + "\n").encode())

    def stop_requested(self):
        try:
            os.stat("stop", dir_fd=self.dfd, follow_symlinks=False)
            return True
        except FileNotFoundError:
            return False


def parse(argv):
    ap = argparse.ArgumentParser(description="只读 artifact 观察器（见文件头限制说明）")
    ap.add_argument("--target", required=True, help="cargo target 目录的绝对、无符号链接路径")
    ap.add_argument("--out-dir", required=True, help="尚不存在的新输出目录（将以 0700 独占创建）")
    ap.add_argument("--artifact", action="append", default=[], help="精确 artifact 绝对路径，可重复")
    ap.add_argument("--artifacts-file", help="每行一个精确 artifact 绝对路径；须为 regular 文件、UTF-8、≤1MiB、≤4096 行")
    ap.add_argument("--interval-ms", type=int, default=10)
    ap.add_argument("--max-seconds", type=float, default=3600,
                    help="观察循环时长，范围 (0, 7200]；从 ready 行输出后开始计时，启动校验与基线不计入")
    ap.add_argument("--hash-every-sample", action="store_true", help="每次采样都 hash（慢，但可发现元数据不变的内容改动）")
    a = ap.parse_args(argv)
    if not 1 <= a.interval_ms <= 1000:
        raise Fail("bad_interval", "--interval-ms 必须在 1..1000")
    if not 0 < a.max_seconds <= 7200:
        raise Fail("bad_max_seconds", "--max-seconds 必须在 (0, 7200]")
    return a


def read_list(path):
    parts = components(path, "artifacts-file")
    if not parts:
        raise Fail("bad_component", "artifacts-file 不能是 /")
    dfd = walk_dirs(parts[:-1], "artifacts-file 目录")
    try:
        fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | getattr(os, "O_CLOEXEC", 0), dir_fd=dfd)
    except OSError as e:
        raise Fail("artifacts_file", f"无法安全打开 artifacts-file: {e.strerror}")
    finally:
        os.close(dfd)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise Fail("artifacts_file_not_regular", "artifacts-file 必须是普通文件（拒绝 FIFO/设备等）")
        data = b""
        while len(data) <= LIST_MAX_BYTES and (chunk := os.read(fd, 1 << 16)):
            data += chunk
    finally:
        os.close(fd)
    if len(data) > LIST_MAX_BYTES:
        raise Fail("artifacts_file_too_large", f"artifacts-file 超过 {LIST_MAX_BYTES} 字节")
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        raise Fail("artifacts_file_not_utf8", "artifacts-file 不是合法 UTF-8")
    lines = [ln for ln in text.split("\n") if ln and not ln.startswith("#")]
    if len(lines) > LIST_MAX_LINES:
        raise Fail("artifacts_file_too_many_lines", f"artifacts-file 超过 {LIST_MAX_LINES} 行")
    return lines


def setup(a):
    tfd = walk_dirs(components(a.target, "target"), "target")
    check_owner(tfd, "target")
    tst = os.fstat(tfd)
    oparts = components(a.out_dir, "out-dir")
    if not oparts:
        raise Fail("bad_component", "out-dir 不能是 /")
    pfd = walk_dirs(oparts[:-1], "out-dir 的父目录", watch=(tst.st_dev, tst.st_ino))
    try:
        check_owner(pfd, "out-dir 的父目录", need_private_write=True)
        paths = list(a.artifact) + (read_list(a.artifacts_file) if a.artifacts_file else [])
        arts = open_artifacts(a.target, tfd, paths)
        try:
            os.mkdir(oparts[-1], 0o700, dir_fd=pfd)
        except FileExistsError:
            raise Fail("out_exists", f"out-dir 已存在（不复用、不覆写）: {a.out_dir}")
        dfd = os.open(oparts[-1], DIR_FLAGS, dir_fd=pfd)
    finally:
        os.close(pfd)
    os.fchmod(dfd, 0o700)
    check_owner(dfd, "out-dir")
    return tfd, arts, Out(dfd)


def observe(a, arts, out):
    stop = {"signal": None}
    for sg in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sg, lambda signum, _f: stop.__setitem__("signal", signum))
    prev = {}
    for art in arts:
        prev[art["path"]] = stable_snap(art, True)
        if "unstable" in prev[art["path"]]:
            raise Fail("baseline_unstable", f"基线快照不稳定: {art['path']}: {prev[art['path']]['unstable']}")
        out.event({"kind": "baseline", "path": art["path"], "new": prev[art["path"]]})
    out.write_json("ready", {"status": "ready", "pid": os.getpid()})
    print(json.dumps({"status": "ready", "out_dir": a.out_dir, "artifacts": len(arts)}), flush=True)
    start = time.monotonic_ns()
    deadline = start + int(a.max_seconds * 1e9)
    samples, changes, last, max_gap = 0, 0, start, 0
    reason, final_stable = None, True
    while reason is None:
        time.sleep(a.interval_ms / 1000)
        now = time.monotonic_ns()
        max_gap, last = max(max_gap, now - last), now
        samples += 1
        for art in arts:
            try:
                cur = snap(art, a.hash_every_sample)
                old = prev[art["path"]]
                changed = comparable(cur) != comparable(old) or (
                    a.hash_every_sample and cur.get("sha256") != old.get("sha256"))
                if changed:
                    if cur["exists"] and "sha256" not in cur:
                        cur = stable_snap(art, True)
                    out.event({"kind": "change", "path": art["path"], "old": old, "new": cur})
                    prev[art["path"]] = cur
                    changes += 1
            except Detached as e:
                out.event({"kind": "path_detached", "path": art["path"], "reason": str(e)})
                reason, final_stable = "path_detached", False
                break
        if reason is not None:
            break
        if stop["signal"] is not None:
            reason = "signal"
        elif out.stop_requested():
            reason = "stop_file"
        elif time.monotonic_ns() >= deadline:
            reason = "max_seconds"
    if reason != "path_detached":
        for art in arts:
            try:
                fin = stable_snap(art, True)
            except Detached as e:
                out.event({"kind": "path_detached", "path": art["path"], "reason": str(e)})
                reason, final_stable = "path_detached", False
                break
            out.event({"kind": "final", "path": art["path"], "new": fin})
            final_stable = final_stable and "unstable" not in fin
    out.write_json("summary.json", {
        "status": "stopped", "stop_reason": reason, "final_stable": final_stable, "samples": samples,
        "change_events": changes, "interval_ms": a.interval_ms, "max_observed_sample_gap_ns": max_gap,
        "duration_ns": time.monotonic_ns() - start, "artifacts": [x["path"] for x in arts],
        "limitations": LIMITATIONS})
    if not final_stable:
        return 5
    return 4 if reason == "max_seconds" else 0


def main(argv):
    out = None
    try:
        a = parse(argv)
        _tfd, arts, out = setup(a)
        return observe(a, arts, out)
    except Fail as e:
        if out is None:
            print(json.dumps({"status": "error", "code": e.code, "message": e.message}, ensure_ascii=False),
                  file=sys.stderr)
            return 2
        err = {"status": "error", "code": e.code, "message": e.message}
    except Detached as e:
        err = {"status": "error", "code": "path_detached", "message": str(e)}
    except OSError as e:
        err = {"status": "error", "code": "os_error", "message": f"{e.strerror}: {e.filename}"}
        if out is None:
            print(json.dumps(err, ensure_ascii=False), file=sys.stderr)
            return 2
    print(json.dumps(err, ensure_ascii=False), file=sys.stderr)
    try:
        out.write_json("error.json", err)
    except OSError:
        pass
    return 3


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
