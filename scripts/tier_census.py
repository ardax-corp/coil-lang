#!/usr/bin/env python3
"""Body tier census: which execution tier each function body gets.

Compiles each file with `coil compile --opt-stats` and reads the per-body
lines (`name: dense|lir|fuse (dense: why; lir: why)`).

    # count tiers and refusal reasons, save the per-body result
    scripts/tier_census.py run --coil target/release/coil \
        --root ../coil-stdlib/src -o new.json examples/perf tests/positive

    # compare two saved runs; exits 1 when any body drops a tier
    scripts/tier_census.py compare old.json new.json

Environment variables set for `run` (for example `COIL_HIR_MIR=0`) reach the
compiler, so one binary can be compared against itself under a switch.
"""

import argparse
import collections
import json
import os
import re
import subprocess
import sys
import tempfile

RANK = {"fuse": 0, "lir": 1, "dense": 2}
BODY = re.compile(r"^    (?P<name>.+): (?P<tier>dense|lir|fuse) \((?P<why>.*)\)$")


def sources(paths):
    for path in paths:
        if os.path.isdir(path):
            for dirpath, _, names in sorted(os.walk(path)):
                for name in sorted(names):
                    if name.endswith(".hy"):
                        yield os.path.join(dirpath, name)
        else:
            yield path


def bodies(coil, roots, path, out):
    cmd = [coil, "compile", "--opt-stats", "--include-tests"]
    for root in roots:
        cmd += ["--root", root]
    cmd += [path, "-o", out]
    try:
        done = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
    except subprocess.TimeoutExpired:
        return None
    if done.returncode != 0:
        return None
    seen = collections.Counter()
    result = {}
    for line in (done.stdout + done.stderr).splitlines():
        m = BODY.match(line)
        if not m:
            continue
        name = m["name"]
        seen[name] += 1
        key = name if seen[name] == 1 else f"{name}#{seen[name]}"
        result[key] = {"tier": m["tier"], "why": m["why"]}
    return result


def reasons(why):
    """The dense and LIR refusals of one body, digits folded."""
    out = []
    for part in why.split("; "):
        tier, _, text = part.partition(": ")
        if text and text != "-":
            out.append(f"{tier}: {re.sub(r'[0-9]+', '#', text)}")
    return out


def run(args):
    census = {}
    failed = []
    with tempfile.TemporaryDirectory() as tmp:
        out = os.path.join(tmp, "out.hyc")
        for path in sources(args.paths):
            found = bodies(args.coil, args.root, path, out)
            if found is None:
                failed.append(path)
            else:
                census[path] = found
    tiers = collections.Counter()
    why = collections.Counter()
    for found in census.values():
        for body in found.values():
            tiers[body["tier"]] += 1
            if body["tier"] != "dense":
                why.update(reasons(body["why"]))
    total = sum(tiers.values())
    print(f"{len(census)} files, {total} bodies: " + ", ".join(f"{tiers[t]} {t}" for t in ("dense", "lir", "fuse")))
    if failed:
        print(f"{len(failed)} files did not compile: " + " ".join(failed[:10]))
    print("top refusals:")
    for text, count in why.most_common(args.top):
        print(f"  {count:6} {text}")
    if args.output:
        with open(args.output, "w") as f:
            json.dump(census, f, indent=1, sort_keys=True)


def compare(args):
    with open(args.old) as f:
        old = json.load(f)
    with open(args.new) as f:
        new = json.load(f)
    up, down, gone = [], [], 0
    for path, bodies_old in old.items():
        bodies_new = new.get(path)
        if bodies_new is None:
            gone += len(bodies_old)
            continue
        for name, body in bodies_old.items():
            other = bodies_new.get(name)
            if other is None:
                gone += 1
                continue
            step = RANK[other["tier"]] - RANK[body["tier"]]
            row = (path, name, body["tier"], other["tier"], other["why"])
            if step > 0:
                up.append(row)
            elif step < 0:
                down.append(row)
    print(f"{len(up)} bodies up a tier, {len(down)} down, {gone} not found in the new run")
    for label, rows in (("down", down), ("up", up)):
        for path, name, a, b, why in rows[: args.top]:
            print(f"  {label}: {path} {name}: {a} -> {b}" + (f" ({why})" if label == "down" else ""))
    return 1 if down else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="compile files and count body tiers")
    r.add_argument("--coil", default="target/release/coil")
    r.add_argument("--root", action="append", default=[], help="module root (repeatable)")
    r.add_argument("-o", "--output", help="save the per-body result as JSON")
    r.add_argument("--top", type=int, default=15, help="refusal reasons to list")
    r.add_argument("paths", nargs="+", help=".hy files or directories")
    c = sub.add_parser("compare", help="compare two saved runs")
    c.add_argument("old")
    c.add_argument("new")
    c.add_argument("--top", type=int, default=40, help="changed bodies to list per direction")
    args = parser.parse_args()
    if args.cmd == "run":
        run(args)
        return 0
    return compare(args)


if __name__ == "__main__":
    sys.exit(main())
