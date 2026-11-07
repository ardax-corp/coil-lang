#!/usr/bin/env python3
"""Compare every level and engine against the raw reference, program by program.

    scripts/mir_diff.py [--bin DIR] [--root DIR ...] [--levels 0,1,2,3,s,g] [PATH ...]

`coil FILE --mir` runs MIR as lowered at `-O 0` (docs/internals/mir-interp.md).
Each `.coil` with a `main` under PATH (default: examples) runs that way, then
at each level on the interpreter and as native code. Timings in the
output (`1234 ns`) are masked. A difference in stdout or exit status from the
reference on the interpreter is a MIR or HIR pass's; on native code alone, at a level
where the interpreter agrees, the backend's; on every engine at once it may be a
program whose output is not deterministic. Exits 1 when any program differs.
"""

import argparse
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

TIMING = re.compile(rb"\d+(\.\d+)? ?(ns|us|ms|s)\b")
ENGINES = {"mir": ["--mir"], "jit": []}


def run(bin_dir, roots, path, flags, timeout):
    cmd = [str(bin_dir / "coil"), str(path), "--no-build-cache", "--allow-all"]
    for r in roots:
        cmd += ["--root", r]
    try:
        p = subprocess.run(cmd + flags, capture_output=True, timeout=timeout, stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        return ("timeout", b"")
    return (p.returncode, TIMING.sub(b"<time>", p.stdout))


def compare(args, path):
    raw = run(args.bin, args.root, path, ["--mir", "-O", "0"], args.timeout)
    if raw[0] == "timeout":
        return (path, "skipped", ["raw run timed out"])
    if b"" == raw[1] and raw[0] not in (0, 1):
        return (path, "crashed", [f"raw exit {raw[0]}"])
    out = []
    for level in args.levels.split(","):
        for name, flags in ENGINES.items():
            if name == "mir" and level == "0":
                continue
            got = run(args.bin, args.root, path, flags + ["-O", level], args.timeout)
            if got != raw:
                what = f"exit {got[0]} vs {raw[0]}" if got[0] != raw[0] else "stdout differs"
                out.append(f"{name} -O{level}: {what}")
    return (path, "differs" if out else "same", out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="*", default=["examples"])
    ap.add_argument("--bin", type=Path, default=Path("target/release"))
    ap.add_argument("--root", action="append", default=[])
    ap.add_argument("--levels", default="0,1,2,3,s,g")
    ap.add_argument("--timeout", type=int, default=60)
    ap.add_argument("-j", type=int, default=8)
    args = ap.parse_args()
    files = []
    for p in map(Path, args.paths):
        found = p.rglob("*.coil") if p.is_dir() else [p]
        files += [f for f in sorted(found) if "fn main(" in f.read_text(errors="replace")]
    with ThreadPoolExecutor(args.j) as pool:
        results = list(pool.map(lambda f: compare(args, f), files))
    counts = {}
    for path, state, notes in results:
        counts[state] = counts.get(state, 0) + 1
        if state != "same":
            print(f"{state.upper():8} {path}: {'; '.join(notes)}")
    print(", ".join(f"{n} {s}" for s, n in sorted(counts.items())) or "no programs")
    return 1 if counts.get("differs") or counts.get("crashed") else 0


if __name__ == "__main__":
    sys.exit(main())
