//! Coverage for `compile_dissect`: grants and effects.

use compiler::Pipeline;

#[test]
fn compile_dissect_attach_matches_compile_grant() {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("coil_dissect_attach_{pid}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let entry = dir.join("main.coil");
    std::fs::write(
        &entry,
        "use io::{stdout};\nfn main() { let _ = stdout().attach(0, 0, 0, 0, 0); }\n",
    )
    .expect("write entry");

    let mut denied = Pipeline::new();
    denied.bind_workspace_language_roots();
    assert!(
        denied
            .compile_dissect(entry.to_str().unwrap())
            .is_err(),
        "attach without grant should fail dissect"
    );
    assert!(
        denied
            .messages()
            .iter()
            .any(|m| m.code() == Some(compiler::ErrorCode::HostAttachDenied)),
        "expected HostAttachDenied, messages={:?}",
        denied
            .messages()
            .iter()
            .map(|m| m.code())
            .collect::<Vec<_>>()
    );

    let mut granted = Pipeline::new();
    granted.bind_workspace_language_roots();
    granted.grant_attach();
    granted
        .compile_dissect(entry.to_str().unwrap())
        .expect("attach with grant should dissect");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `coil dissect --effects`: each function's effects with the reason, and
/// why auto-par left a loop sequential.
#[test]
fn effects_capture_names_reasons_and_auto_par_blockers() {
    let src = r#"
static let HITS: int = 0;
fn step(int i) -> int {
    HITS = HITS + 1;
    return i * 2;
}
fn sq(int i) -> int {
    return i * i;
}
fn apply(int x, int -> int f) -> int {
    return f(x);
}
fn main() {
    let acc = 0;
    for i in 0..1000 {
        acc = acc + step(i);
    }
    let b = 0;
    for i in 0..1000 {
        b = b + apply(i, fn (int x) => sq(x));
    }
    HITS = acc + b;
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    compiler::start_effects_capture();
    pipeline.compile_src(src).expect("compiles");
    let fx = compiler::take_effects_capture();
    let of = |name: &str| {
        fx.fns
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.as_str())
            .unwrap_or_else(|| panic!("no `{name}` in {:?}", fx.fns))
    };
    assert_eq!(
        of("step"),
        "uses {read, mutate}: writes static `HITS` (mutate); reads static `HITS` (read)"
    );
    assert_eq!(of("sq"), "pure");
    assert_eq!(of("apply"), "pure apart from its parameters: calls parameter `f`");
    assert_eq!(
        fx.auto_par,
        vec!["loop over `i` in `main` not parallelized: `step` writes static `HITS` (mutate)".to_string()],
        "{fx:?}"
    );
}

/// `coil dissect --effects` also prints each function's escape facts.
#[test]
fn effects_capture_lists_escape_facts() {
    let src = r#"
enum Tree {
    Leaf,
    Node(Tree, Tree),
}
static let SEEN: Vec<Tree> = Vec::new();
fn bottom_up(int depth) -> Tree {
    if depth == 0 {
        return Tree::Leaf();
    }
    return Tree::Node(bottom_up(depth - 1), bottom_up(depth - 1));
}
fn item_check(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(left, right) => 1 + item_check(left) + item_check(right),
    };
}
fn keep(Tree t) {
    SEEN.push(t);
}
fn main() {
    keep(bottom_up(2));
    let _ = item_check(bottom_up(3));
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    compiler::start_effects_capture();
    pipeline.compile_src(src).expect("compiles");
    let fx = compiler::take_effects_capture();
    let of = |name: &str| {
        fx.escape
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.as_str())
            .unwrap_or_else(|| panic!("no `{name}` in {:?}", fx.escape))
    };
    assert_eq!(of("bottom_up"), "result fresh");
    assert_eq!(of("item_check"), "keeps nothing");
    assert_eq!(of("keep"), "keeps `t`");
}

/// `coil dissect --boxes`: each place lowering boxes a value, by function,
/// kind and type; a direct two-word call boxes nothing.
#[test]
fn box_capture_names_function_kind_and_type() {
    let src = r#"
fn half(int x) -> Option<int> {
    if x % 2 == 0 {
        return Some(x / 2);
    }
    return None;
}
fn direct(int n) -> int {
    return match half(n) {
        Some(v) => v,
        None => 0,
    };
}
fn stored(int n) -> int {
    let v: Vec<Option<int>> = Vec::new();
    v.push(half(n));
    return v.len();
}
fn main() {
    let _ = direct(4) + stored(3);
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let of = |name: &str| boxes.iter().filter(|b| b.func == name).map(|b| (b.kind, b.ty.as_str())).collect::<Vec<_>>();
    assert_eq!(of("direct"), vec![], "{boxes:?}");
    assert_eq!(of("stored"), vec![(compiler::BoxKind::Pair, "Option<int>")], "{boxes:?}");
    assert!(boxes.iter().all(|b| b.site.starts_with("from_hir.rs:")), "{boxes:?}");
}

/// A generic class's method at a pair type argument gets its own clone,
/// which takes and returns the pair in two words: neither the caller nor
/// the clone boxes it.
#[test]
fn box_capture_pair_type_argument_clone_has_no_boxes() {
    let src = r#"
class Pass<T> { pub n: int }
impl Pass<T> {
    pub fn keep(T x) -> T { return x; }
}
fn pairs(int n) -> int {
    let p: Pass<(int, int)> = new Pass(0);
    let (a, b) = p.keep((n, n + 1));
    return a + b;
}
fn options(int n) -> int {
    let p: Pass<Option<int>> = new Pass(0);
    return match p.keep(Some(n)) {
        Some(v) => v,
        None => 0,
    };
}
fn main() {
    let _ = pairs(1) + options(2);
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes
        .iter()
        .filter(|b| b.func == "pairs" || b.func == "options" || b.func.starts_with("Pass::"))
        .collect();
    assert!(mine.is_empty(), "{mine:?}");
}

/// A host native's two-word result (`Vec::pop` of `Option<int>`) comes
/// back as its two words: the caller boxes nothing.
#[test]
fn box_capture_host_pair_result_has_no_boxes() {
    let src = r#"
fn drain(Vec<int> v) -> int {
    let s = 0;
    let more = true;
    while more {
        match v.pop() {
            Some(x) => {
                s += x;
            },
            None => {
                more = false;
            },
        }
    }
    return s;
}
fn main() {
    let _ = drain(Vec::from([1, 2, 3]));
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| b.func == "drain").collect();
    assert!(mine.is_empty(), "{mine:?}");
}

/// `==` on niche words (`Option<string>`) and two-word values
/// (`Option<int>`, `(int, float)`) compares their words: no side is boxed.
#[test]
fn box_capture_eq_on_words_has_no_boxes() {
    let src = r#"
#[inline(never)]
fn name(int i) -> Option<string> {
    if i == 0 {
        return None;
    }
    return Some("a");
}
#[inline(never)]
fn half(int x) -> Option<int> {
    if x % 2 == 0 {
        return Some(x / 2);
    }
    return None;
}
#[inline(never)]
fn point(int x) -> (int, float) {
    return (x, 0.5);
}
fn same(int i) -> bool {
    return name(i) == Some("a") && half(i) != None && point(i) == (2, 0.5);
}
fn main() {
    let _ = same(2);
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| b.func == "same").collect();
    assert!(mine.is_empty(), "{mine:?}");
}
