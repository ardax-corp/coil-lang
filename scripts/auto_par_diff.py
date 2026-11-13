#!/usr/bin/env python3
"""Run every program with auto-par on and off and compare what it leaves.

The rule (`docs/internals/auto-par.md`): a program's stdout, stderr and exit
code with automatic fork-join are those of `--auto-par off`. Each program
runs as its own `coil <file>` process, once each way per engine, with
`COIL_MAX_WORKER_THREADS` set (default 8) so the forks really run on
several workers: under cargo the pool has one.

    scripts/auto_par_diff.py --release -- --root examples/src --root .deps/coil-stdlib/src
    scripts/auto_par_diff.py --release tests/numeric examples/perf/fib.coil -- …
    scripts/auto_par_diff.py --bin harness-bin --engine native -- …

The programs are the files with a `fn main` under `tests/` (not
`compile_fail/`), `examples/*.coil` and `examples/perf/*.coil`, or under the
paths given. A file's `// @flags` header is added to the flags after `--`,
and `// @needs` is honoured, as `coil test` does. Two header lines name
this cell (`auto-par`):

    // @skip auto-par <why>            not run: its output depends on scheduling or time
    // @xfail auto-par #<issue> <why>  a known difference: it must differ, and
                                       the run fails once it does not

When the two ways differ the sequential run is repeated twice: if it
differs from itself the program is reported as `unstable` (it needs a
`@skip` line), else as `differs` (a bug). Exits 1 on either, on a timeout, and on
an `@xfail` file that is the same both ways.
"""

import argparse
import os
import re
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ".exe" if os.name == "nt" else ""
CELL = "auto-par"
MAIN = re.compile(r"^(?:pub\s+)?fn\s+main\s*\(", re.M)
DIRECTIVE = re.compile(r"^//\s*@([A-Za-z0-9_-]+):?\s?(.*)$")
# Features every platform this script runs on has (`coil test` knows the list).
NEEDS = {"sockets", "symlinks", "ffi", "processes"}
ENGINES = {"native": [], "mir": ["--mir"]}
# Every clause checked, as `coil test` does, and no call computed at compile
# time: a folded call never forks.
FLAGS = ["--no-build-cache", "--contracts", "all", "--fold-calls", "off"]


def header(src):
    """`{name: [values]}` of the `// @name value` lines before the first code line."""
    found = {}
    for line in src.splitlines():
        if not line.startswith("//"):
            break
        m = DIRECTIVE.match(line)
        if m:
            found.setdefault(m.group(1).lower(), []).append(m.group(2))
    return found


def for_cell(values):
    """The text after `auto-par` on the first line that names this cell."""
    for value in values:
        name, _, why = value.strip().partition(" ")
        if name == CELL:
            return why.strip()
    return None


def programs(paths):
    """Every `.coil` file with a `fn main` under `paths`, sorted."""
    files = []
    for path in paths:
        path = Path(path)
        if path.is_dir():
            files += sorted(path.rglob("*.coil"))
        else:
            files.append(path)
    return [f for f in files if "compile_fail" not in f.parts]


def default_paths():
    return [Path("tests"), *sorted(Path("examples").glob("*.coil")), *sorted(Path("examples/perf").glob("*.coil"))]


def main():
    argv = sys.argv[1:]
    passthrough = []
    if "--" in argv:
        k = argv.index("--")
        argv, passthrough = argv[:k], argv[k + 1 :]
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("path", nargs="*", help="files or directories (default: tests, examples, examples/perf)")
    ap.add_argument("-p", "--parallel", type=int, default=max(1, (os.cpu_count() or 2) // 2),
                    help="programs run at once (default: half the CPUs)")
    ap.add_argument("--workers", type=int, default=8, help="COIL_MAX_WORKER_THREADS of every run (default 8)")
    ap.add_argument("--engine", action="append", choices=sorted(ENGINES),
                    help="engine to run on (repeatable; default: both)")
    ap.add_argument("--release", action="store_true",
                    help="build and run target/release instead of target/debug")
    ap.add_argument("--no-build", action="store_true", help="do not run cargo")
    ap.add_argument("--bin", type=Path, help="directory with coil")
    ap.add_argument("--timeout", type=float, default=120, help="seconds one run may take (default 120)")
    ap.add_argument("-v", "--verbose", action="store_true", help="list every program, and the skipped ones with why")
    args = ap.parse_args(argv)

    bindir = args.bin or ROOT / "target" / ("release" if args.release else "debug")
    if not args.no_build and not args.bin:
        build = ["cargo", "build", "--bin", "coil"]
        subprocess.run(build + (["--release"] if args.release else []), cwd=ROOT, check=True)
    coil = (bindir / f"coil{EXE}").resolve()
    if not coil.exists():
        sys.exit(f"{coil} not found (build it or pass --bin)")
    engines = args.engine or ["native", "mir"]
    env = dict(os.environ, COIL_MAX_WORKER_THREADS=str(args.workers))

    os.chdir(ROOT)
    files = programs(args.path or default_paths())

    def observe(file, engine, flags, auto_par):
        cmd = [str(coil), *FLAGS, *ENGINES[engine], "--auto-par", auto_par, *passthrough, *flags, str(file)]
        try:
            out = subprocess.run(cmd, cwd=ROOT, env=env, stdin=subprocess.DEVNULL, capture_output=True,
                                 timeout=args.timeout)
        except subprocess.TimeoutExpired:
            return None
        return (out.stdout, out.stderr, out.returncode)

    def run(file):
        """`(file, verdict, detail)`: verdict is skip / same / differs / unstable / timeout / xfail / fixed / error."""
        src = file.read_text(errors="replace")
        if not MAIN.search(src):
            return file, None, ""
        head = header(src)
        missing = [n for v in head.get("needs", []) for n in v.split() if n not in NEEDS and not (n == "unix" and os.name == "posix")]
        if missing:
            return file, "skip", f"needs {' '.join(missing)}"
        skip = for_cell(head.get("skip", []))
        if skip is not None:
            if not skip:
                return file, "error", f"`// @skip {CELL}` gives no reason"
            return file, "skip", skip
        known = for_cell(head.get("xfail", []))
        if known is not None and not re.search(r"#\d+\b", known):
            return file, "error", f"`// @xfail {CELL}` names no issue: write `// @xfail {CELL} #<issue> <why>`"
        flags = [w for v in head.get("flags", []) for w in v.split()]
        verdict, detail = "same", ""
        for engine in engines:
            off = observe(file, engine, flags, "off")
            on = observe(file, engine, flags, "on")
            if off is None or on is None:
                which = " and ".join(w for w, r in (("off", off), ("on", on)) if r is None)
                verdict, detail = "timeout", f"{engine}: --auto-par {which} ran past {args.timeout:.0f}s"
                break
            if on == off:
                continue
            # Is it the program? Two more sequential runs must agree with the first.
            again = next((r for r in (observe(file, engine, flags, "off") for _ in range(2)) if r != off), off)
            if again != off:
                other = diff(off, again, "off", "off again") if again else f"one ran past {args.timeout:.0f}s"
                verdict, detail = "unstable", f"{engine}: two --auto-par off runs differ\n{other}"
            else:
                verdict, detail = "differs", f"{engine} (three --auto-par off runs agree): {diff(off, on, 'off', 'on')}"
            break
        if known is not None:
            if verdict == "same":
                return file, "fixed", f"the same both ways, but `// @xfail {CELL} {known}` lists it: remove that line"
            return file, "xfail", known
        return file, verdict, detail

    start = time.monotonic()
    print(f"auto-par on vs off: {len(files)} files, {' and '.join(engines)}, "
          f"{args.workers} workers, {args.parallel} at a time", flush=True)
    with ThreadPoolExecutor(max_workers=max(1, args.parallel)) as pool:
        results = [r for r in pool.map(run, files) if r[1] is not None]
    count = lambda verdict: sum(1 for r in results if r[1] == verdict)
    bad = [r for r in results if r[1] in ("differs", "unstable", "timeout", "fixed", "error")]
    for file, verdict, detail in results:
        if verdict in ("skip", "xfail") and args.verbose:
            print(f"{verdict:<8} {file} ({detail})")
        elif verdict == "same" and args.verbose:
            print(f"ok       {file}")
    for file, verdict, detail in bad:
        print(f"\n---- {file}: {verdict} ----\n{detail}")
    if bad:
        print("\nfailed:")
        for file, verdict, _ in bad:
            print(f"    {verdict:<8} {file}")
    known = f"; {count('xfail')} known failures" if count("xfail") else ""
    print(f"\nauto-par: {'FAILED' if bad else 'ok'}. {count('same')} same; {count('differs')} differ; "
          f"{count('unstable')} unstable; {count('timeout')} timed out{known}; {count('skip')} skipped; "
          f"{time.monotonic() - start:.1f}s")
    return 1 if bad else 0


def diff(a, b, a_name, b_name):
    """What differs between two runs' `(stdout, stderr, code)`."""
    lines = []
    for what, x, y in zip(("stdout", "stderr"), a, b):
        if x != y:
            lines.append(f"{what} with --auto-par {a_name}:\n{clip(x)}\n{what} with --auto-par {b_name}:\n{clip(y)}")
    if a[2] != b[2]:
        lines.append(f"exit code {a[2]} with --auto-par {a_name}, {b[2]} with --auto-par {b_name}")
    return "\n".join(lines)


def clip(data, limit=1500):
    text = data.decode(errors="replace")
    return text if len(text) <= limit else text[:limit] + f"… ({len(text) - limit} more bytes)"


if __name__ == "__main__":
    sys.exit(main())
