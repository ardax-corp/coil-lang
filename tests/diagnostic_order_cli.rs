//! A diagnostic is the same text on every run: nothing in it depends on the
//! order a hash map happens to iterate in (#2464).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

/// Compile `main.coil` next to the other `files` `runs` times, each in its
/// own process (its own hash seeds), and count each distinct stderr.
fn compile_outputs(tag: &str, files: &[(&str, &str)], runs: usize) -> BTreeMap<String, usize> {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir: PathBuf = std::env::temp_dir().join(format!("coil-diag-order-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, src) in files {
        std::fs::write(dir.join(name), src).unwrap();
    }
    let mut seen = BTreeMap::new();
    for _ in 0..runs {
        let out = Command::new(&bin)
            .arg("compile")
            .arg(dir.join("main.coil"))
            .arg("--no-build-cache")
            .arg("--root")
            .arg(&dir)
            .arg("-o")
            .arg(dir.join("main.ccoil"))
            .output()
            .expect("spawn coil");
        assert!(!out.status.success(), "the program must be rejected");
        *seen.entry(String::from_utf8_lossy(&out.stderr).into_owned()).or_insert(0) += 1;
    }
    let _ = std::fs::remove_dir_all(&dir);
    seen
}

const STORE: &str = "\
class Ring {
    pub slots: [int; 4],
    pub at: int,
}

impl Ring {
    pub fn put(int v) {
        self.slots[self.at] = v;
    }
}

class Tally {
    pub n: int,
}

impl Tally {
    pub fn put(int v) {
        self.n = self.n + v;
    }
}

class Log {
    pub items: Vec<int>,
}

impl Log {
    pub fn put(int v) {
        self.items.push(v);
    }
}
";

/// E0107 for a method of another module says why *that* method mutates
/// `self`, not why a same-named method of another class does, and says it
/// on every run.
#[test]
fn e0107_reason_names_the_called_method_on_every_run() {
    let main = "\
use store::{Ring, Tally, Log};

fn main() {
    let t = readonly new Tally(0);
    t.put(1);
}
";
    let seen = compile_outputs("method", &[("store.coil", STORE), ("main.coil", main)], 24);
    assert_eq!(seen.len(), 1, "one output over 24 runs, got: {seen:#?}");
    let err = seen.keys().next().unwrap();
    assert!(err.contains("E0107") && err.contains("(writes field `n`)"), "{err}");
}

const TRAITS: &str = "\
class Tally {
    pub n: int,
    pub items: Vec<int>,
}

class Ring {
    pub slots: [int; 4],
}

trait Fill<T> {
    fn put(T x, int v) -> unit {}
}

trait Load<T> {
    fn put(T x, int v) -> unit {}
}

impl Fill for Tally {
    pub fn put(Tally x, int v) -> unit {
        x.n = v;
    }
}

impl Load for Tally {
    pub fn put(Tally x, int v) -> unit {
        x.items.push(v);
    }
}

impl Fill for Ring {
    pub fn put(Ring x, int v) -> unit {
        x.slots[0] = v;
    }
}
";

/// `Trait::m(x)` with two traits that both have an `m` implemented for the
/// class: the reason is the named trait's impl's, in the module that
/// declares them and in one that imports them.
#[test]
fn e0107_reason_names_the_called_trait_impl_on_every_run() {
    let calls = "
fn main() {
    let t = readonly new Tally(0, [1]);
    Load::put(t, 2);
}
";
    let local = format!("{TRAITS}{calls}");
    let seen = compile_outputs("trait-local", &[("main.coil", &local)], 24);
    assert_eq!(seen.len(), 1, "one output over 24 runs, got: {seen:#?}");
    let err = seen.keys().next().unwrap();
    assert!(err.contains("E0107") && err.contains("(calls `.push()` on a `Vec`)"), "{err}");

    let imported = format!("use store::{{Tally, Ring, Fill, Load}};\n{calls}");
    let seen = compile_outputs("trait-imported", &[("store.coil", TRAITS), ("main.coil", &imported)], 24);
    assert_eq!(seen.len(), 1, "one output over 24 runs, got: {seen:#?}");
    let err = seen.keys().next().unwrap();
    assert!(err.contains("E0107") && err.contains("(calls `.push()` on a `Vec`)"), "{err}");
}
