"""Machine identification + simple resource sampling for the bench report.

Not the real HardwareProbe from PLAN-DECIDE D0-3 (that is Rust, lives in
``agent24-decide``, and this bench must not depend on it) — just enough to
label a `results/<machine-id>/` directory and report peak RSS per
candidate, per the D0-5 task description.
"""

from __future__ import annotations

import platform
import subprocess


def _sysctl(name: str) -> str | None:
    try:
        out = subprocess.run(["sysctl", "-n", name], capture_output=True, text=True, timeout=5)
        if out.returncode == 0:
            return out.stdout.strip()
    except Exception:  # noqa: BLE001
        pass
    return None


def default_machine_id() -> str:
    """Best-effort ``<chip><ram>g`` slug, e.g. ``m1max-64g``. Callers should
    still prefer an explicit ``--machine`` since this cannot know the
    marketing name ("Max" vs "Pro") from sysctl alone in every case."""
    chip = _sysctl("machdep.cpu.brand_string") or platform.processor() or "unknown"
    mem_bytes = _sysctl("hw.memsize")
    ram_gb = int(int(mem_bytes) / (1024**3)) if mem_bytes else 0
    chip_slug = chip.lower().replace("apple ", "").replace(" ", "")
    return f"{chip_slug}-{ram_gb}g"


def machine_info() -> dict[str, str]:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cpu_brand": _sysctl("machdep.cpu.brand_string") or platform.processor() or "unknown",
        "ram_bytes": _sysctl("hw.memsize") or "unknown",
    }
