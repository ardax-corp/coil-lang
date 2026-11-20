//! E0808: a two-word value the project's own code boxes where no layout
//! keeps its two words (a class-parameter field, a bare-trait pack, a
//! `forall` function value's argument), and quiet everywhere else.

use compiler::Pipeline;
use reporting::ErrorCode;

/// `(message, help)` of each E0808 warning in `src`.
fn residue(src: &str) -> Vec<(String, String)> {
    let mut pipeline = Pipeline::new();
    pipeline.compile_src(src).expect("compile");
    pipeline
        .messages()
        .iter()
        .filter(|m| m.code() == Some(ErrorCode::BoxedResidue))
        .map(|m| (m.message().to_string(), m.help().clone().unwrap_or_default()))
        .collect()
}

#[test]
fn class_parameter_field_warns_once() {
    let notes = residue("
class Cell<V> {
    pub v: V,
}

fn main() {
    let a = new Cell(Some(1));
    let b = new Cell(Some(2));
    if a.v != b.v {
        let c = new Cell(3);
    }
}
");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].0.contains("`Option<int>`") && notes[0].0.contains("field `v`"), "{notes:?}");
}

#[test]
fn concrete_pair_field_is_quiet() {
    let notes = residue("
class Cell<V> {
    pub v: V,
    pub o: Option<int>,
}

fn main() {
    let a = new Cell(1, Some(1));
    if a.o == None {
        panic \"o\";
    }
}
");
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn bare_trait_pack_warns() {
    let notes = residue("
trait Named<T> {
    fn label(readonly T self) -> string {}
}

enum Pick {
    Num(int),
    Off,
}

impl Named for Pick {
    pub fn label(readonly Pick self) -> string {
        return \"x\";
    }
}

fn main() {
    let s: Named = Pick::Num(3);
}
");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].0.contains("bare trait type"), "{notes:?}");
}

#[test]
fn forall_value_argument_warns_and_named_call_is_quiet() {
    let notes = residue("
fn id<T>(T x) -> T {
    return x;
}

fn once(forall T. T -> T f, Option<int> x) -> Option<int> {
    return f(x);
}

fn main() {
    if once(id, Some(4)) != id(Some(4)) {
        panic \"id\";
    }
}
");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].0.contains("`forall` function value"), "{notes:?}");
}

#[test]
fn ordinary_pair_code_is_quiet() {
    let notes = residue("
fn half(int n) -> Option<int> {
    return if n % 2 == 0 { Some(n / 2) } else { None };
}

fn main() {
    let v: Vec<int> = [1, 2, 3, 4];
    let sum = 0;
    for x in v {
        if let Some(h) = half(x) {
            sum += h;
        }
    }
    let r: Result<int, string> = Ok(sum);
    if half(sum) != None || r != Ok(3) {
        panic \"sum\";
    }
}
");
    assert!(notes.is_empty(), "{notes:?}");
}
