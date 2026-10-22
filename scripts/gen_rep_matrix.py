#!/usr/bin/env python3
"""Generate the value-representation matrix (`tests/representation/`).

One `.hy` program per (representation, position) cell: it builds two values
of the representation, moves each through the position (a boundary where a
value can change hands: a generic method, a `Vec<V>` field, a closure, a
thread, a host call, a dictionary in a shared body) and prints what ground
code then reads. The expected text is the ground reading of the same values,
so every cell checks a round trip with `// @output`.

    scripts/gen_rep_matrix.py                 # (re)write tests/representation/
    scripts/gen_rep_matrix.py --check         # fail if the tree is stale (CI)
    scripts/gen_rep_matrix.py --probe [BIN]   # run every cell, print the matrix
    scripts/gen_rep_matrix.py --probe --update-baseline

A cell listed in `rep_matrix_baseline.json` (next to this script) as red is
written as what it does today: a refusal goes under `compile_fail/` with its
`@expected` codes, a wrong result or a crash is written under
`scripts/rep_matrix_known_wrong/` (outside the corpora), so CI stays green
and the baseline is the record. `--probe` compiles and runs every cell (in
a scratch directory, at the default level, `-O 0`, `COIL_HIR_INLINE=0` and
`COIL_HIR_SPECIALIZE=0`) and
reports how each one ends; `--update-baseline` writes what it saw.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "tests" / "representation"
WRONG = ROOT / "scripts" / "rep_matrix_known_wrong"
BASELINE = ROOT / "scripts" / "rep_matrix_baseline.json"

# The issue a red cell's failure is filed as, by its detail text.
ISSUES = [
    (r"callee-generic", "#1888"),
    (r"open-niche-element", "#1896"),
    (r"no MIR from HIR", "#1903"),
    (r"not sendable", "#1904"),
    (r"host Option/Result layout mismatch", "#1905"),
]


def issue_of(detail):
    for pat, issue in ISSUES:
        if re.search(pat, detail or ""):
            return issue
    return None


class Rep:
    """A representation: its type, two values, the declarations it needs and
    the ground reading `say(T v) -> string`, with the expected reading of
    each value."""

    def __init__(self, key, ty, a, b, say, expect, decls="", traits=("Eq", "Show", "Hash")):
        self.key = key
        self.ty = ty
        self.a = a
        self.b = b
        self.say = say
        self.expect = expect
        self.decls = decls
        self.traits = traits


OBJ = """#[derive(Eq, Hash, Show)]
class Obj {
    pub n: int,
}
"""

PAIR_CLASS = """readonly class P {
    pub x: float,
    pub y: float,
}
"""

LVL = """#[repr(int)]
#[derive(Eq, Hash, Show)]
enum Lvl {
    Low = 1,
    High = 2,
}
"""

HELD = """#[derive(Eq, Hash, Show)]
enum Res {
    Held(int),
    Gone,
}

impl Res {
    fn drop() {}
}
"""

REPS = [
    Rep("int", "int", "7", "-3", 'return format("int %i", v);', ("int 7", "int -3")),
    Rep("object", "Obj", "new Obj(1)", "new Obj(2)", 'return format("obj %i", v.n);', ("obj 1", "obj 2"), OBJ),
    Rep("string", "string", '"one"', '"two"', 'return "str " + v;', ("str one", "str two")),
    Rep(
        "option_string",
        "Option<string>",
        'Some("one")',
        "None",
        'return match v {\n        Some(s) => "some " + s,\n        None => "none",\n    };',
        ("some one", "none"),
        traits=("Eq", "Show"),
    ),
    Rep(
        "result_string",
        "Result<string, string>",
        'Ok("one")',
        'Err("bad")',
        'return match v {\n        Ok(s) => "ok " + s,\n        Err(e) => "err " + e,\n    };',
        ("ok one", "err bad"),
        traits=("Eq", "Show"),
    ),
    Rep(
        "option_int",
        "Option<int>",
        "Some(7)",
        "None",
        'return match v {\n        Some(n) => format("some %i", n),\n        None => "none",\n    };',
        ("some 7", "none"),
        traits=("Eq", "Show"),
    ),
    Rep(
        "result_unit",
        "Result<(), string>",
        "Ok(())",
        'Err("bad")',
        'return match v {\n        Ok(_) => "ok",\n        Err(e) => "err " + e,\n    };',
        ("ok", "err bad"),
        traits=("Eq", "Show"),
    ),
    Rep(
        "tuple_pair",
        "(int, float)",
        "(1, 2.5)",
        "(3, 0.5)",
        'let (x, y) = v;\n    return format("tuple %i %v", x, y);',
        ("tuple 1 2.5", "tuple 3 0.5"),
    ),
    Rep(
        "pair_class",
        "P",
        "new P(1.5, 2.0)",
        "new P(0.5, 4.0)",
        'return format("p %v %v", v.x, v.y);',
        ("p 1.5 2.0", "p 0.5 4.0"),
        PAIR_CLASS,
        ("Eq", "Hash"),
    ),
    Rep(
        "scalar_enum",
        "Lvl",
        "Lvl::High",
        "Lvl::Low",
        'return match v {\n        Lvl::Low => "low",\n        Lvl::High => "high",\n    };',
        ("high", "low"),
        LVL,
    ),
    Rep(
        "option_option",
        "Option<Option<string>>",
        'Some(Some("one"))',
        "Some(None)",
        'return match v {\n        Some(Some(s)) => "some some " + s,\n        Some(None) => "some none",\n        None => "none",\n    };',
        ("some some one", "some none"),
        traits=("Eq", "Show"),
    ),
    Rep(
        "drop_enum",
        "Res",
        "Res::Held(4)",
        "Res::Gone",
        'return match v {\n        Res::Held(n) => format("held %i", n),\n        Res::Gone => "gone",\n    };',
        ("held 4", "gone"),
        HELD,
    ),
]


class Pos:
    """A position: declarations (generic over `V` where it is a boundary of
    a generic) and the expression reading value `{X}` back through it."""

    def __init__(self, key, read, decls="", uses=(), needs=None, out=None):
        self.key = key
        self.read = read
        self.decls = decls
        self.uses = uses
        self.needs = needs
        self.out = out


BOX = """class Box<V> {
    pub v: V,
    pub vs: Vec<V>,
}

impl Box<V> {
    pub fn put(V x) {
        self.v = x;
        self.vs.push(x);
    }

    pub fn get() -> V {
        return self.v;
    }

    pub fn at(int i) -> V {
        return self.vs[i];
    }
}

fn boxed({T} x) -> Box<{T}> {
    let vs: Vec<{T}> = Vec::new();
    let c = new Box(x, vs);
    c.put(x);
    return c;
}
"""

KBOX = """class KBox<K, V> {
    pub ks: Vec<K>,
    pub vs: Vec<V>,
}

impl KBox<K: Eq, V> {
    pub fn put(K k, V x) {
        self.ks.push(k);
        self.vs.push(x);
    }

    pub fn get(K k) -> V {
        let i = 0;
        while i < self.ks.len() - 1 && self.ks[i] != k {
            i = i + 1;
        }
        return self.vs[i];
    }
}

fn kboxed({T} x) -> KBox<string, {T}> {
    let ks: Vec<string> = Vec::new();
    let vs: Vec<{T}> = Vec::new();
    let c = new KBox(ks, vs);
    c.put("k", x);
    return c;
}
"""

POSITIONS = [
    Pos("ground_arg", "pass({X})", "fn pass({T} v) -> {T} {\n    let w = v;\n    return w;\n}\n"),
    Pos("ground_return", "made_{N}()", None),
    Pos("generic_arg", "boxed({X}).at(0)", BOX),
    Pos("generic_return", "boxed({X}).get()", BOX),
    Pos("bounded_arg", 'kboxed({X}).get("k")', KBOX),
    Pos("bare_field", "boxed({X}).v", BOX),
    Pos("vec_field", "boxed({X}).vs[0]", BOX),
    Pos("vec_field_local", "first(boxed({X}).vs)", BOX + "\nfn first(Vec<{T}> w) -> {T} {\n    return w[0];\n}\n"),
    Pos(
        "hashmap_get",
        "mapped({X})",
        "fn mapped({T} x) -> {T} {\n    let m: HashMap<string, {T}> = HashMap::new();\n    m.insert(\"k\", x);\n    return m.get(\"k\", x);\n}\n",
        ("collections::map::HashMap",),
    ),
    Pos(
        "hashmap_values",
        "valued({X})",
        "fn valued({T} x) -> {T} {\n    let m: HashMap<string, {T}> = HashMap::new();\n    m.insert(\"k\", x);\n    return m.values()[0];\n}\n",
        ("collections::map::HashMap",),
    ),
    Pos(
        "hashmap_entry",
        "entried({X})",
        "fn entried({T} x) -> {T} {\n    let m: HashMap<string, {T}> = HashMap::new();\n    m.insert(\"k\", x);\n    for e in m {\n        return e.value;\n    }\n    return x;\n}\n",
        ("collections::map::HashMap",),
    ),
    Pos(
        "tuple_item",
        "untupled({X})",
        "fn tupled<W>(W w) -> (W, int) {\n    return (w, 1);\n}\n\nfn untupled({T} x) -> {T} {\n    let (y, _) = tupled(x);\n    return y;\n}\n",
    ),
    Pos("record_field", "recorded({X})", "fn recorded({T} x) -> {T} {\n    let r = { v: x, n: 1 };\n    return r.v;\n}\n"),
    Pos(
        "closure_capture",
        "captured({X})",
        "fn captured({T} x) -> {T} {\n    let f = fn () use (x) => x;\n    return f();\n}\n",
    ),
    Pos(
        "generic_lambda",
        None,
        "fn apply<W>(W w, W -> string f) -> string {\n    return f(w);\n}\n",
        out="apply({X}, say)",
    ),
    Pos(
        "thread_join",
        "threaded({X})",
        "fn threaded({T} x) -> {T} {\n    let th = match spawn(fn () use (x) => x) {\n        Ok(t) => t,\n        Err(_) => panic \"spawn\",\n    };\n    return match join(th) {\n        Ok(v) => v,\n        Err(_) => panic \"join\",\n    };\n}\n",
        ("thread::{join, spawn}",),
    ),
    Pos(
        "host_pop",
        "popped({X})",
        "fn popped({T} x) -> {T} {\n    let vs: Vec<{T}> = [x];\n    return match vs.pop() {\n        Some(v) => v,\n        None => panic \"empty\",\n    };\n}\n",
    ),
    Pos(
        "dict_eq",
        None,
        "fn same<W: Eq>(W a, W b) -> bool {\n    return a == b;\n}\n",
        needs="Eq",
        out='format("%v %v", same({X}, {X}), same({X}, {Y}))',
    ),
    Pos(
        "dict_show",
        None,
        "fn shown<W: Show>(W w) -> string {\n    return w.show();\n}\n",
        needs="Show",
        out='format("%v", shown({X}) == {X}.show())',
    ),
    Pos(
        "dict_hash",
        None,
        "fn hashed<W: Hash>(W w) -> int {\n    return w.hash();\n}\n",
        needs="Hash",
        out='format("%v", hashed({X}) == {X}.hash())',
    ),
]


def expected(rep, pos):
    if pos.key == "dict_eq":
        return ["true false", "true false"]
    if pos.key in ("dict_show", "dict_hash"):
        return ["true", "true"]
    return list(rep.expect)


def program(rep, pos):
    """The cell's source and expected stdout lines."""
    out = expected(rep, pos)
    uses = ["io::sync::println", "string::format"] + list(pos.uses)
    lines = ["// @output " + "\\n".join(out) + "\\n"]
    lines.append(f"//")
    lines.append(f"// Representation matrix cell {rep.key} x {pos.key}: generated by")
    lines.append(f"// scripts/gen_rep_matrix.py, do not edit.")
    for u in sorted(set(uses)):
        lines.append(f"use {u};")
    lines.append("")
    if rep.decls:
        lines.append(rep.decls)
    lines.append(f"fn say({rep.ty} v) -> string {{\n    {rep.say}\n}}\n")
    decls = (pos.decls or "").replace("{T}", rep.ty)
    if pos.key == "ground_return":
        decls = (
            f"fn made_a() -> {rep.ty} {{\n    return {rep.a};\n}}\n\n"
            f"fn made_b() -> {rep.ty} {{\n    return {rep.b};\n}}\n"
        )
    if decls:
        lines.append(decls)
    lines.append("fn main() {")
    lines.append(f"    let a: {rep.ty} = {rep.a};")
    lines.append(f"    let b: {rep.ty} = {rep.b};")
    for x, y, n in (("a", "b", "a"), ("b", "a", "b")):
        if pos.out:
            expr = pos.out.replace("{X}", x).replace("{Y}", y)
            lines.append(f"    println({expr})?;")
        else:
            expr = pos.read.replace("{X}", x).replace("{N}", n)
            lines.append(f"    println(say({expr}))?;")
    lines.append("}")
    return "\n".join(lines) + "\n", out


def cells():
    for rep in REPS:
        for pos in POSITIONS:
            if pos.needs and pos.needs not in rep.traits:
                continue
            yield rep, pos


def name(rep, pos):
    return f"{rep.key}__{pos.key}.hy"


def load_baseline():
    if BASELINE.exists():
        return json.loads(BASELINE.read_text())
    return {}


def render():
    """Every generated file: path -> text."""
    base = load_baseline()
    files = {}
    for rep, pos in cells():
        src, _ = program(rep, pos)
        n = name(rep, pos)
        state = base.get(n[:-3], {"status": "ok"})
        status = state["status"]
        if status == "ok":
            files[OUT / n] = src
        elif status == "refused":
            why = state.get("detail", "")
            if issue_of(why):
                why += f", {issue_of(why)}"
            head = f"// @expected {' or '.join(state['codes'])} — red in the representation matrix baseline ({why})\n"
            body = re.sub(r"^// @output [^\n]*\n", "", src)
            files[OUT / "compile_fail" / n] = head + body
        else:
            why = state.get("detail", "")
            if issue_of(why):
                why = f"{issue_of(why)}: {why}"
            files[WRONG / n] = f"// Known {status}: {why}\n" + src
    return files


def write(files):
    for d in (OUT, WRONG):
        if d.exists():
            shutil.rmtree(d)
    for path, text in files.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)


def check(files):
    have = set()
    for d in (OUT, WRONG):
        if d.exists():
            have |= {p for p in d.rglob("*.hy")}
    stale = [p for p in files if not p.exists() or p.read_text() != files[p]]
    extra = sorted(have - set(files))
    for p in stale + extra:
        print(f"stale: {p.relative_to(ROOT)}")
    return not stale and not extra


def run_cell(bin_dir, src, out, flags, env_extra):
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "cell.hy"
        path.write_text(src)
        env = dict(os.environ, **env_extra)
        try:
            p = subprocess.run(
                [str(bin_dir / "coil"), *flags, str(path)],
                capture_output=True, text=True, timeout=60, env=env,
            )
        except subprocess.TimeoutExpired:
            return {"status": "crash", "detail": "timeout"}
    want = "\n".join(out) + "\n"
    if p.returncode == 0 and p.stdout == want:
        return {"status": "ok"}
    codes = sorted(set(re.findall(r"\[(E\d{4})\]", p.stderr)))
    if p.returncode != 0 and codes and not p.stdout:
        reason = re.findall(r"cannot lower `[^`]+` yet \(([^)]+)\)", p.stderr)
        state = {"status": "refused", "codes": codes}
        if reason:
            state["detail"] = ", ".join(sorted(set(reason)))
        else:
            state["detail"] = p.stderr.strip().splitlines()[0][:160]
        return state
    if p.returncode == 0:
        return {"status": "wrong", "detail": "printed " + repr(p.stdout[:120])}
    err = p.stderr.strip().splitlines() or ["?"]
    tail = err[-1]
    for i, line in enumerate(err):
        if "panicked at" in line and i + 1 < len(err):
            tail = err[i + 1]
            break
    tail = re.sub(r"\(\d+\)", "", tail)[:160]
    return {"status": "crash", "detail": f"exit {p.returncode}: {tail}"}


# The settings CI runs the corpora under (plus the shared-body path with
# specialisation off): a cell is green only when it is green in each.
MODES = [
    ("", [], {}),
    ("-O 0", ["-O", "0"], {}),
    ("COIL_HIR_INLINE=0", [], {"COIL_HIR_INLINE": "0"}),
    ("COIL_HIR_SPECIALIZE=0", [], {"COIL_HIR_SPECIALIZE": "0"}),
]


def probe(bin_dir, flags):
    seen = {}
    for rep, pos in cells():
        src, out = program(rep, pos)
        n = name(rep, pos)[:-3]
        state = {"status": "ok"}
        for label, extra_flags, env in MODES:
            got = run_cell(bin_dir, src, out, extra_flags + flags, env)
            if got["status"] != "ok":
                state = dict(got, detail=(label + ": " if label else "") + got.get("detail", ""))
                break
        seen[n] = state
    return seen


def table(seen):
    mark = {"ok": ".", "refused": "R", "wrong": "W", "crash": "C"}
    keys = [p.key for p in POSITIONS]
    rows = ["| rep | " + " | ".join(keys) + " |", "|---|" + "---|" * len(keys)]
    for rep in REPS:
        cols = []
        for k in keys:
            st = seen.get(f"{rep.key}__{k}")
            cols.append(mark[st["status"]] if st else "-")
        rows.append(f"| {rep.key} | " + " | ".join(cols) + " |")
    return "\n".join(rows)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true", help="fail if the generated tree is stale")
    ap.add_argument("--probe", nargs="?", const=str(ROOT / "target" / "release"), metavar="BIN",
                    help="run every cell with BIN/coil (default target/release)")
    ap.add_argument("--update-baseline", action="store_true", help="with --probe: record what it saw")
    ap.add_argument("--root", action="append", default=[], help="--root for the probe's coil")
    args = ap.parse_args()
    if args.probe:
        roots = args.root or [str(ROOT / "examples" / "src"), str(ROOT.parent / "coil-stdlib" / "src")]
        flags = [f for r in roots for f in ("--root", r)]
        seen = probe(Path(args.probe), flags)
        print(table(seen))
        red = {k: v for k, v in seen.items() if v["status"] != "ok"}
        for k, v in sorted(red.items()):
            print(f"{v['status']:8} {k}: {v.get('detail', '')}")
        print(f"\n{len(seen) - len(red)} of {len(seen)} cells green")
        if args.update_baseline:
            BASELINE.write_text(json.dumps(red, indent=2, sort_keys=True) + "\n")
            write(render())
        return 0
    files = render()
    if args.check:
        return 0 if check(files) else 1
    write(files)
    return 0


if __name__ == "__main__":
    sys.exit(main())
