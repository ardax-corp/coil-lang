//! A loop's bounds proof does not survive user code that may shorten the
//! array, however that code is reached: an operator, an iterator, a static
//! method, a `len` or `push` of the user's, a function value
//! (coil-lang#2438).
//!
//! Each program fills a static `Vec`, walks it in a counted loop, and on
//! the first trip runs `shrink()` (which empties it) by one such route
//! before it reads `xs[i]` into a local it never uses. The read is out of
//! range and has to panic at every level: when a proof marked it in
//! bounds, the optimizer deleted it and the program ran on.

use std::path::Path;
use std::process::Command;

/// One way to reach `shrink()`: declarations, statements before the loop,
/// the statement in the loop, and an extra parameter and argument of `walk`.
struct Route {
    name: &'static str,
    decls: &'static str,
    before: &'static str,
    trigger: &'static str,
    param: &'static str,
    arg: &'static str,
}

const fn route(name: &'static str, decls: &'static str, trigger: &'static str) -> Route {
    Route { name, decls, before: "", trigger, param: "", arg: "" }
}

const ADD: &str = "impl Add for K {
    pub fn add(K a, K b) -> K {
        shrink();
        return a;
    }
}";

const POOL: &str = "class Pool {
    pub n: int,
}
impl Pool {
    pub static fn new(int n) -> int {
        shrink();
        return n;
    }
    pub static fn len() -> int {
        shrink();
        return 1;
    }
    pub static fn with_capacity(int n) -> int {
        shrink();
        return n;
    }
    pub static fn drain(int n) -> int {
        shrink();
        return n;
    }
}";

const LENGTH: &str = "impl Length for K {
    fn len(K b) -> int {
        shrink();
        return 3;
    }
}";

const ROUTES: &[Route] = &[
    // The control: a call by name of a function that resizes.
    route("call", "", "shrink();"),
    route("operator +", ADD, "let z = k + k;"),
    route(
        "operator <",
        "impl Lt for K {
    pub fn lt(K a, K b) -> bool {
        shrink();
        return false;
    }
}",
        "let z = k < k;",
    ),
    route(
        "operator ==",
        "impl Eq for K {
    pub fn eq(K a, K b) -> bool {
        shrink();
        return true;
    }
}",
        "let z = k == k;",
    ),
    route(
        "unary -",
        "impl Neg for K {
    pub fn neg(K a) -> K {
        shrink();
        return a;
    }
}",
        "let z = -k;",
    ),
    Route { name: "operator +=", decls: ADD, before: "let acc = new K(0);", trigger: "acc += k;", param: "", arg: "" },
    route(
        "for-in over an iterator",
        "class Count {
    pub cur: int,
}
impl Iterator for Count {
    type Item = int;
    fn next(Count c) -> Option<int> {
        shrink();
        return Option::None;
    }
}",
        "for v in new Count(0) { trips = trips + v; }",
    ),
    Route {
        name: "for-in over a generator",
        decls: "gen fn ticks() -> int {
    while true {
        shrink();
        yield 1;
    }
}",
        before: "let g = ticks();",
        trigger: "for v in g { trips = trips + v; break; }",
        param: "",
        arg: "",
    },
    route("static method new", POOL, "let z = Pool::new(1);"),
    route("static method len", POOL, "let z = Pool::len();"),
    route("static method with_capacity", POOL, "let z = Pool::with_capacity(1);"),
    route("static method", POOL, "let z = Pool::drain(1);"),
    route(
        "trait static method",
        "impl Default for K {
    pub static fn default() -> K {
        shrink();
        return new K(0);
    }
}",
        "let z: K = K::default();",
    ),
    route("Length::len of a class", LENGTH, "let z = k.len();"),
    route("Length::len(k)", LENGTH, "let z = Length::len(k);"),
    route(
        "inherent len",
        "impl K {
    pub fn len() -> int {
        shrink();
        return 3;
    }
}",
        "let z = k.len();",
    ),
    route(
        "inherent push",
        "impl K {
    pub fn push(int x) {
        shrink();
    }
}",
        "k.push(1);",
    ),
    Route {
        name: "function value named like a function",
        decls: "fn f() {
    let z = 1;
}",
        before: "",
        trigger: "f();",
        param: ", () -> unit f",
        arg: ", fn () => shrink()",
    },
    route(
        "Show in format",
        "impl Show for K {
    pub fn show(K self) -> string {
        shrink();
        return \"k\";
    }
}",
        "let z = format(\"%v\", k);",
    ),
];

/// The program for `route`, with a `for i in 0..xs.len()` loop or a
/// `while i < xs.len()` one.
fn program(route: &Route, counted_for: bool) -> String {
    let Route { decls, before, trigger, param, arg, .. } = route;
    let (head, step) = if counted_for { ("for i in 0..xs.len() {", "") } else { ("let i = 0;\n    while i < xs.len() {", "i = i + 1;") };
    format!(
        "use string::format;

static let G: Vec<int> = Vec::new();

fn shrink() {{
    while G.len() > 0 {{
        G.pop();
    }}
}}

class K {{
    pub v: int,
}}

{decls}

fn walk(Vec<int> xs, K k{param}) -> int {{
    let trips = 0;
    {before}
    {head}
        {trigger}
        let unused = xs[i];
        trips = trips + 1;
        {step}
    }}
    return trips;
}}

fn main() {{
    for i in 0..100 {{
        G.push(7);
    }}
    let trips = walk(G, new K(1){arg});
}}
"
    )
}

/// Runs `src` at `mode`: whether it panicked out of bounds, and its stderr.
fn panics_out_of_bounds(bin: &str, dir: &Path, src: &str, mode: &[&str]) -> (bool, String) {
    let path = dir.join("main.coil");
    std::fs::write(&path, src).unwrap();
    let mut cmd = Command::new(bin);
    cmd.args(mode);
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
    let out = cmd.arg(&path).output().expect("coil run");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (!out.status.success() && stderr.contains("index out of bounds"), stderr)
}

#[test]
fn an_out_of_range_read_after_hidden_user_code_panics() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_bounds_proof_user_code_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut missed = Vec::new();
    for route in ROUTES {
        for counted_for in [true, false] {
            let src = program(route, counted_for);
            for mode in [&[][..], &["-O", "0"][..], &["--mir"][..]] {
                let (panicked, stderr) = panics_out_of_bounds(&bin, &dir, &src, mode);
                if !panicked {
                    let shape = if counted_for { "for" } else { "while" };
                    missed.push(format!("{} ({shape}, {mode:?}): {}", route.name, stderr.lines().next().unwrap_or("no panic")));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(missed.is_empty(), "no out-of-bounds panic:\n{}", missed.join("\n"));
}

/// `i < xs.len()` holds on the way into the loop and on its first trip
/// only: the call at the end of each trip empties `xs`.
const CARRIED: &str = "static let G: Vec<int> = Vec::new();

fn shrink() {
    while G.len() > 0 {
        G.pop();
    }
}

fn walk(Vec<int> xs) -> int {
    let trips = 0;
    let i = 0;
    if i < xs.len() {
        let n = 0;
        while n < 3 {
            let unused = xs[i];
            shrink();
            trips = trips + 1;
            n = n + 1;
        }
    }
    return trips;
}

fn main() {
    for i in 0..100 {
        G.push(7);
    }
    let trips = walk(G);
}
";

#[test]
fn a_fact_carried_into_a_loop_dies_at_a_call_on_its_back_edge() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_bounds_proof_carried_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for mode in [&[][..], &["-O", "0"][..], &["--mir"][..]] {
        let (panicked, stderr) = panics_out_of_bounds(&bin, &dir, CARRIED, mode);
        assert!(panicked, "{mode:?}: no out-of-bounds panic on the second trip; stderr: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
