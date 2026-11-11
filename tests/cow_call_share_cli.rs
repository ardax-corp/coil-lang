//! #2206: `p = f(p)` moves `p` into the call, so updating a container
//! through a `V -> V` function copies nothing. Counted with
//! `COIL_COW_REPORT`, which reports the copies copy-on-write made.

use std::process::Command;

fn coil_bin() -> String {
    std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)")
}

/// The copies `COIL_COW_REPORT` counts for `src` run as a program.
fn copies(name: &str, src: &str) -> u64 {
    let dir = std::env::temp_dir().join(format!("coil_cow_call_share_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.coil");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(coil_bin()).arg("--cow-report").arg(&file).output().expect("spawn coil");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{name} failed: {stderr}");
    let line = stderr.lines().find(|l| l.starts_with("cow-report:")).unwrap_or_else(|| panic!("{name}: no cow-report in {stderr}"));
    line.trim_start_matches("cow-report: ").split_whitespace().next().unwrap().parse().unwrap()
}

const N: u64 = 2000;

#[test]
fn reassigning_a_local_from_a_call_copies_nothing() {
    let src = format!(
        "
fn add(Vec<string> vs) -> Vec<string> {{
    vs.push(\"x\");
    return vs;
}}
fn main() {{
    let vs: Vec<string> = Vec::new();
    let i = 0;
    while i < {N} {{
        vs = add(vs);
        i = i + 1;
    }}
    if vs.len() != {N} {{ panic \"wrong length\"; }}
}}
"
    );
    let n = copies("local", &src);
    assert!(n < 4, "{n} copies for {N} calls");
}

#[test]
fn reassigning_through_a_function_value_copies_nothing() {
    let src = format!(
        "
fn grow(Vec<int> vs, Vec<int> -> Vec<int> f) -> Vec<int> {{
    let i = 0;
    while i < {N} {{
        vs = f(vs);
        i = i + 1;
    }}
    return vs;
}}
fn push_one(Vec<int> v) -> Vec<int> {{
    v.push(1);
    return v;
}}
fn main() {{
    let vs = grow(Vec::new(), push_one);
    if vs.len() != {N} {{ panic \"wrong length\"; }}
}}
"
    );
    let n = copies("lambda", &src);
    assert!(n < 4, "{n} copies for {N} calls");
}

#[test]
fn reassigning_a_field_from_a_call_copies_nothing() {
    let src = format!(
        "
fn add(Vec<int> vs) -> Vec<int> {{
    vs.push(1);
    return vs;
}}
class Bag {{
    pub items: Vec<int>,
}}
fn main() {{
    let b = new Bag(Vec::new());
    let i = 0;
    while i < {N} {{
        b.items = add(b.items);
        i = i + 1;
    }}
    if b.items.len() != {N} {{ panic \"wrong length\"; }}
}}
"
    );
    let n = copies("field", &src);
    assert!(n < 4, "{n} copies for {N} calls");
}

#[test]
fn reassigning_an_element_from_a_call_copies_nothing() {
    let src = format!(
        "
fn add(Vec<int> vs) -> Vec<int> {{
    vs.push(1);
    return vs;
}}
fn main() {{
    let a: Vec<Vec<int>> = [Vec::new(), Vec::new()];
    let i = 0;
    let k = 1;
    while i < {N} {{
        a[k] = add(a[k]);
        i = i + 1;
    }}
    if a[1].len() != {N} || a[0].len() != 0 {{ panic \"wrong length\"; }}
}}
"
    );
    let n = copies("element", &src);
    assert!(n < 4, "{n} copies for {N} calls");
}

#[test]
fn an_entry_style_update_chain_copies_nothing() {
    let src = format!(
        "
fn push_one(Vec<int> v) -> Vec<int> {{
    v.push(1);
    return v;
}}
class Store {{
    pub vals: Vec<Vec<int>>,
}}
view class Slot {{
    mut store: Store,
    at: int,
}}
impl Slot {{
    pub fn and_modify(Vec<int> -> Vec<int> f) -> Slot {{
        self.store.vals[self.at] = f(self.store.vals[self.at]);
        return self;
    }}
    pub fn or_insert(Vec<int> init) -> Vec<int> {{
        return self.store.vals[self.at];
    }}
}}
fn main() {{
    let s = new Store([Vec::new(), Vec::new()]);
    let i = 0;
    while i < {N} {{
        let _ = new Slot(mut s, i % 2).and_modify(push_one).or_insert(Vec::new());
        i = i + 1;
    }}
    if s.vals[0].len() != {N} / 2 {{ panic \"wrong length\"; }}
}}
"
    );
    let n = copies("entry", &src);
    assert!(n < 4, "{n} copies for {N} calls");
}

#[test]
fn a_generic_trait_call_result_that_is_only_read_copies_nothing() {
    let src = format!(
        "
trait Pieces<P> {{
    fn pieces(P self, string s) -> Vec<string> {{}}
}}
impl Pieces for string {{
    fn pieces(string self, string s) -> Vec<string> {{
        let v: Vec<string> = Vec::new();
        v.push(s);
        v.push(self);
        return v;
    }}
}}
fn split_with<P: Pieces>(P p, string s) -> Vec<string> {{
    return p.pieces(s + \"!\");
}}
fn main() {{
    let n = 0;
    let i = 0;
    while i < {N} {{
        let parts = split_with(\" \", \"a b\");
        n = n + parts.len();
        i = i + 1;
    }}
    if n != 2 * {N} {{ panic \"wrong length\"; }}
}}
"
    );
    let n = copies("generic", &src);
    assert!(n < 4, "{n} copies for {N} calls");
}

#[test]
fn a_static_method_storing_its_argument_copies_nothing() {
    let src = format!(
        "
class S {{
    pub a: Vec<int>,
    pub b: Vec<int>,
}}
class H {{
    pub s: S,
    pub i: int,
}}
impl H {{
    pub static fn wrap(S s, int i) -> H {{
        return new H(s, i);
    }}
}}
fn main() {{
    let s = new S(Vec::new(), Vec::new());
    s.a.push(1);
    let t = 0;
    let i = 0;
    while i < {N} {{
        let h = H::wrap(s, i);
        t = t + h.s.a[0];
        i = i + 1;
    }}
    if t != {N} {{ panic \"wrong sum\"; }}
}}
"
    );
    let n = copies("static", &src);
    assert!(n < 4, "{n} copies for {N} calls");
}
