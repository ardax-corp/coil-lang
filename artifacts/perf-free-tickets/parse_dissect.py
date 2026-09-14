#!/usr/bin/env python3
"""Histogram opcodes from `coil dissect` text (investigation helper)."""

from __future__ import annotations

import re
import sys
from collections import Counter, defaultdict

FN_RE = re.compile(r"^;; fn (\S+)\s+bc\[(\d+)\.\.(\d+)\)")
OP_RE = re.compile(r"^(\d+)\s+(\S+)\s*(.*)$")
INDEX_OPS = {
    "Index",
    "IndexUnchecked",
    "IndexPin",
    "IndexPinUnchecked",
    "StoreIndex",
    "StoreIndexUnchecked",
    "StoreIndexPin",
    "StoreIndexPinUnchecked",
    "DenseIndex",
    "DenseStoreIndex",
    "ArrayPin",
    "DenseArrayLen",
    "ArrayLen",
}


def parse(text: str) -> dict:
    by_fn: dict[str, Counter] = defaultdict(Counter)
    current = "<module>"
    total = Counter()
    dense_index = []
    for line in text.splitlines():
        m = FN_RE.match(line.strip())
        if m:
            current = m.group(1)
            continue
        m = OP_RE.match(line.strip())
        if not m:
            continue
        pc, op, rest = m.group(1), m.group(2), m.group(3).strip()
        by_fn[current][op] += 1
        total[op] += 1
        if op in ("DenseIndex", "DenseStoreIndex"):
            hx = None
            hm = re.search(r"op=0x([0-9a-fA-F]+)", rest)
            if hm:
                hx = int(hm.group(1), 16)
            flags = (hx >> 24) & 0xFF if hx is not None else None
            dense_index.append(
                {
                    "fn": current,
                    "pc": int(pc),
                    "op": op,
                    "operand": hx,
                    "flags": flags,
                    "unchecked": bool(flags & 1) if flags is not None else None,
                    "raw": rest,
                }
            )
    return {"by_fn": by_fn, "total": total, "dense_index": dense_index}


def fmt_counter(c: Counter, n: int = 20) -> str:
    lines = []
    for op, k in c.most_common(n):
        lines.append(f"  {k:5d}  {op}")
    return "\n".join(lines)


def main() -> None:
    text = sys.stdin.read()
    data = parse(text)
    print("=== total opcode histogram ===")
    print(fmt_counter(data["total"], 40))
    print("\n=== index-family counts ===")
    tot = data["total"]
    for op in sorted(INDEX_OPS):
        if tot[op]:
            print(f"  {tot[op]:5d}  {op}")
    print("\n=== DenseIndex / DenseStoreIndex flags ===")
    for row in data["dense_index"]:
        u = row["unchecked"]
        print(
            f"  {row['fn']:20s} pc={row['pc']:5d} {row['op']:16s} "
            f"flags={row['flags']!r} unchecked={u} {row['raw']}"
        )
    print("\n=== per-function (index family + CALL/Seek/HostInvoke) ===")
    keys = (
        "CALL",
        "TailCall",
        "Seek",
        "HostInvoke",
        "FORMAT",
        "LOAD",
        "STORE",
        "DenseBin",
        "DenseMove",
        "DenseConst",
        "BinSlotSlotJmpf",
        "BinSlotImmJmpf",
        "JMP",
    )
    for fn, c in sorted(data["by_fn"].items()):
        idx = {k: c[k] for k in INDEX_OPS if c[k]}
        hot = {k: c[k] for k in keys if c[k]}
        if not idx and not hot:
            continue
        print(f"\n[{fn}]")
        if idx:
            print("  index:", idx)
        if hot:
            print("  other:", hot)


if __name__ == "__main__":
    main()
