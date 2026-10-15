#!/usr/bin/env python3
"""Run the `.hy` language corpora (`tests/<corpus>/`) in parallel.

Each corpus is its own `coil test` process, so a slow corpus does not hold
up the others and a `-j 1` run (the leak smoke) still uses every core.

    scripts/test_corpora.py                       # build, then every corpus
    scripts/test_corpora.py patterns traits       # just these corpora
    scripts/test_corpora.py -- -O 0 --root examples/src   # flags for coil test
    scripts/test_corpora.py --shard 1/3           # a third of the corpora (CI)

The binaries come from `cargo build` (`--release`: `cargo build --release`,
the same build mode as shipped binaries). Cargo's own cache makes a rebuild
with no source change a no-op; `--no-build` skips cargo
entirely and `--bin DIR` runs the `coil` / `coil-test` already in DIR.
`--harness PATH` runs that binary instead of `coil test` (the GC-stress
build of `coil-test`). `--limit-as-kb` caps each process's address space
like `ulimit -v` (the leak smoke). Exits 1 when any corpus fails.
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TESTS = ROOT / "tests"
EXE = ".exe" if os.name == "nt" else ""


def corpora():
    """Every directory under tests/ holding `.hy` files, largest first."""
    found = []
    for d in sorted(TESTS.iterdir()):
        if d.is_dir():
            n = sum(1 for _ in d.rglob("*.hy"))
            if n:
                found.append((n, d.name))
    found.sort(key=lambda t: -t[0])
    return [name for _, name in found]


def shard(names, spec):
    """`I/N` (1-based): round-robin over the size-sorted corpora."""
    i, n = (int(x) for x in spec.split("/"))
    if not 1 <= i <= n:
        sys.exit(f"bad --shard {spec}")
    return names[i - 1 :: n]


def main():
    argv = sys.argv[1:]
    passthrough = []
    if "--" in argv:
        k = argv.index("--")
        argv, passthrough = argv[:k], argv[k + 1 :]
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("corpus", nargs="*", help="corpus names (default: all)")
    ap.add_argument("-p", "--parallel", type=int, default=os.cpu_count() or 2,
                    help="corpora run at once (default: CPUs)")
    ap.add_argument("-j", "--jobs", type=int, default=0,
                    help="`coil test -j` per corpus (default: CPUs / --parallel)")
    ap.add_argument("--shard", help="I/N: run every Nth corpus starting at I")
    ap.add_argument("--release", action="store_true",
                    help="build and run target/release instead of target/debug")
    ap.add_argument("--no-build", action="store_true", help="do not run cargo")
    ap.add_argument("--bin", type=Path, help="directory with coil / coil-test")
    ap.add_argument("--harness", type=Path, help="run this coil-test binary directly")
    ap.add_argument("--limit-as-kb", type=int,
                    help="address-space cap per coil test process (leak smoke; POSIX)")
    ap.add_argument("--logs", type=Path, default=ROOT / "target" / "corpora",
                    help="per-corpus logs (default: target/corpora)")
    args = ap.parse_args(argv)

    names = args.corpus or corpora()
    unknown = [n for n in names if not (TESTS / n).is_dir()]
    if unknown:
        sys.exit(f"no such corpus: {', '.join(unknown)}")
    if args.shard:
        names = shard(names, args.shard)

    profile = "release" if args.release else "debug"
    bindir = args.bin or ROOT / "target" / profile
    if not args.no_build and not args.bin and not args.harness:
        build = ["cargo", "build", "--bin", "coil", "--bin", "coil-test"]
        subprocess.run(build + (["--release"] if args.release else []), cwd=ROOT, check=True)
    if args.harness:
        cmd = [str(args.harness)]
    else:
        coil = bindir / f"coil{EXE}"
        if not coil.exists():
            sys.exit(f"{coil} not found (build it or pass --bin)")
        cmd = [str(coil), "test"]

    parallel = max(1, min(args.parallel, len(names)))
    jobs = args.jobs or max(1, (os.cpu_count() or 2) // parallel)
    if args.logs.exists():
        shutil.rmtree(args.logs)
    args.logs.mkdir(parents=True)
    print(f"running {len(names)} corpora, {parallel} at a time, -j {jobs} each", flush=True)

    limit = None
    if args.limit_as_kb:
        import resource

        def limit():
            cap = args.limit_as_kb * 1024
            resource.setrlimit(resource.RLIMIT_AS, (cap, cap))

    def run(name):
        log = args.logs / f"{name}.log"
        start = time.monotonic()
        with open(log, "wb") as out:
            code = subprocess.run(
                cmd + [f"tests/{name}", "-j", str(jobs)] + passthrough,
                cwd=ROOT, stdout=out, stderr=subprocess.STDOUT, preexec_fn=limit).returncode
        secs = time.monotonic() - start
        text = log.read_text(errors="replace")
        m = re.search(r"test result: (\w+)\. (\d+) passed; (\d+) failed", text)
        summary = f"{m.group(2)} passed, {m.group(3)} failed" if m else "no summary"
        ok = code == 0 and m is not None and m.group(1) == "ok"
        print(f"{'ok    ' if ok else 'FAILED'} {name:<13} {summary:<24} {secs:6.1f}s", flush=True)
        return name, ok, text

    start = time.monotonic()
    with ThreadPoolExecutor(max_workers=parallel) as pool:
        results = list(pool.map(run, names))
    failed = [r for r in results if not r[1]]
    for name, _, text in failed:
        print(f"\n===== {name} (tests/{name}) =====")
        lines = [l for l in text.splitlines() if not l.startswith("ok ")]
        print("\n".join(lines[-200:]))
    total = time.monotonic() - start
    verdict = "FAILED" if failed else "ok"
    print(f"\ncorpora: {verdict}. {len(results) - len(failed)} passed; "
          f"{len(failed)} failed; {total:.1f}s (logs in {args.logs})")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
