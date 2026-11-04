#!/usr/bin/env python3
"""Sum what native runs still leave on the VM (`docs/internals/native.md`).

    COIL_NATIVE_INVENTORY=inv.tsv scripts/test_corpora.py -- --engine native
    scripts/native_inventory.py inv.tsv [--exe PACKAGED] [--max KEY=N ...]

`coil-test --engine native` appends a line per compiled file to the file
`COIL_NATIVE_INVENTORY` names (bodies compiled and refused, each refusal
with its reason) and a `counts` line per run (VM entries, bails, calls the
VM ran, host calls in a VM stub, bails by trap code: `bail_trap_0` is an
operation native code does not model, `bail_trap_6` the stack budget). This prints the totals and the refusals
grouped by reason, and with `--exe` the number of interpreter symbols
(`machine::vm`) in a packaged executable. `--max KEY=N` fails when a total
goes over N (keys: refused, failed, vm_entries, bails, vm_runs,
host_via_vm, vm_symbols); the plan's steps lower them to 0. With
`GITHUB_STEP_SUMMARY` set the report also goes to the job summary.
"""

import argparse
import collections
import os
import re
import subprocess
import sys


def reason_kind(why):
    """A refusal's reason without the names and numbers that vary."""
    why = re.sub(r"`[^`]*`", "`…`", why)
    why = re.sub(r"\b\d+\b", "N", why)
    return why[:120]


def vm_symbols(exe):
    out = subprocess.run(["nm", "-C", exe], capture_output=True, text=True, check=True).stdout
    return sum(1 for line in out.splitlines() if "machine::vm::" in line)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("inventory")
    ap.add_argument("--exe", help="packaged executable to count interpreter symbols in")
    ap.add_argument("--max", action="append", default=[], metavar="KEY=N")
    ap.add_argument("--top", type=int, default=25)
    args = ap.parse_args()

    totals = collections.Counter()
    reasons = collections.Counter()
    examples = {}
    failed = []
    try:
        lines = open(args.inventory, encoding="utf-8", errors="replace").read().splitlines()
    except FileNotFoundError:
        lines = []
    for line in lines:
        f = line.split("\t")
        if f[0] == "file" and len(f) >= 4:
            totals["files"] += 1
            totals["compiled"] += int(f[2])
            totals["refused"] += int(f[3])
        elif f[0] == "refused" and len(f) >= 4:
            kind = reason_kind(f[3])
            reasons[kind] += 1
            examples.setdefault(kind, f"{f[1]}: {f[2]}")
        elif f[0] == "failed" and len(f) >= 3:
            totals["failed"] += 1
            failed.append(f"{f[1]}: {f[2]}")
        elif f[0] == "counts":
            for kv in f[2:]:
                k, _, v = kv.partition("=")
                if v.isdigit():
                    totals[k] += int(v)
    if args.exe:
        totals["vm_symbols"] = vm_symbols(args.exe)

    out = ["## Native inventory", ""]
    keys = ["files", "compiled", "refused", "failed", "vm_entries", "bails", "vm_runs", "host_via_vm", "vm_symbols"]
    out += ["| total | count |", "| --- | ---: |"]
    keys += sorted(k for k in totals if k.startswith("bail_trap_"))
    out += [f"| {k} | {totals[k]} |" for k in keys if k in totals or k in ("refused", "failed")]
    if reasons:
        out += ["", f"### Refusals by reason (top {args.top})", "", "| bodies | reason | first seen |", "| ---: | --- | --- |"]
        for kind, n in reasons.most_common(args.top):
            out.append(f"| {n} | {kind.replace('|', '/')} | {examples[kind].replace('|', '/')} |")
    if failed:
        out += ["", "### Backend failures", ""] + [f"- {x}" for x in failed[: args.top]]
    report = "\n".join(out) + "\n"
    print(report)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as s:
            s.write(report)

    over = []
    for spec in args.max:
        k, _, n = spec.partition("=")
        if totals[k] > int(n):
            over.append(f"{k} {totals[k]} > {n}")
    if over:
        print("error: over the limit: " + ", ".join(over), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
