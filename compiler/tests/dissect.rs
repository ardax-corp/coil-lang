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
/// kind and type; a direct two-word call and a `Vec` of pairs box nothing.
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
    // `Vec::new()` of `Option<int>` is a lane array: the push keeps the
    // value's two words.
    assert_eq!(of("stored"), vec![], "{boxes:?}");
    // A host result is still boxed where the native returns one word.
    assert_eq!(of("Vec::pop"), vec![(compiler::BoxKind::HostResult, "vec_pop -> Option<int>")], "{boxes:?}");
    assert!(boxes.iter().all(|b| b.site.starts_with("from_hir.rs:")), "{boxes:?}");
}

/// The `Vec` methods on a lane array work on its lanes: no function here
/// turns the array into boxed elements. Passing it to a generic body
/// does.
#[test]
fn box_capture_lane_vec_methods_keep_the_lanes() {
    let src = r#"
fn methods(int n) -> int {
    let v: Vec<Option<int>> = Vec::with_capacity(4);
    v.push(Some(n));
    v.insert(0, None);
    v.reserve(2);
    let t = v.capacity();
    let _ = v.remove(1);
    let _ = v.pop();
    v.clear();
    return t + v.len();
}
#[inline(never)]
fn count<T>(Vec<T> v) -> int {
    let n = 0;
    for _ in v {
        n += 1;
    }
    return n;
}
fn generic(int n) -> int {
    let v: Vec<Option<int>> = [Some(n)];
    return count(v);
}
fn main() {
    let _ = methods(3) + generic(4);
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let lanes = |name: &str| boxes.iter().filter(|b| b.func == name && b.kind == compiler::BoxKind::LaneArray).count();
    assert_eq!(lanes("methods"), 0, "{boxes:?}");
    assert_eq!(lanes("generic"), 1, "{boxes:?}");
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

/// A named function used as a value returns its two words to direct calls
/// and through the value: only its one-word adapter (`f$value`) boxes.
#[test]
fn box_capture_named_fn_value_boxes_only_in_its_adapter() {
    let src = r#"
fn half(int x) -> Option<int> {
    if x % 2 != 0 {
        return None;
    }
    return Some(x / 2);
}
#[inline(never)]
fn through(int -> Option<int> f, int x) -> int {
    return match f(x) {
        Some(v) => v,
        None => -1,
    };
}
fn main() {
    let _ = through(half, 4) + match half(2) {
        Some(v) => v,
        None => 0,
    };
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| ["half", "through", "main"].contains(&b.func.as_str())).collect();
    assert!(mine.is_empty(), "{mine:?}");
    assert!(boxes.iter().any(|b| b.func == "half$value"), "{boxes:?}");
}

/// A partial application of a function with a two-word result targets the
/// function's one-word adapter: the function returns its two words to direct
/// calls and to calls through the partial, and only the adapter boxes.
#[test]
fn box_capture_partial_of_pair_fn_boxes_only_in_its_adapter() {
    let src = r#"
fn add_opt(int a, int b) -> Option<int> {
    if a + b < 0 {
        return None;
    }
    return Some(a + b);
}
#[inline(never)]
fn through(int -> Option<int> f, int x) -> int {
    return match f(x) {
        Some(v) => v,
        None => -1,
    };
}
fn main() {
    let p = add_opt(10);
    let _ = through(p, 2) + match add_opt(1, 2) {
        Some(v) => v,
        None => 0,
    };
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| ["add_opt", "through", "main"].contains(&b.func.as_str())).collect();
    assert!(mine.is_empty(), "{mine:?}");
    assert!(boxes.iter().any(|b| b.func == "add_opt$value"), "{boxes:?}");
}

/// An inherent instance method's two-word result goes back to its direct
/// calls as two words, as a free function's does (#2333): no box.
#[test]
fn box_capture_method_pair_result_has_no_boxes() {
    let src = r#"
class Bag {
    pub base: int,
}
impl Bag {
    #[inline(never)]
    pub fn at(int k) -> Option<int> {
        if k < 0 {
            return None;
        }
        return Some(self.base + k);
    }
    #[inline(never)]
    pub fn split(int k) -> (int, int) {
        return (self.base, k);
    }
}
fn main() {
    let b = new Bag(1);
    let (x, y) = b.split(2);
    let _ = x + y + match b.at(3) {
        Some(v) => v,
        None => 0,
    };
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| ["Bag::at", "Bag::split", "main"].contains(&b.func.as_str())).collect();
    assert!(mine.is_empty(), "{mine:?}");
}

/// A partial application of such a method targets its one-word adapter,
/// as a free function's does: only the adapter boxes.
#[test]
fn box_capture_partial_of_pair_method_boxes_only_in_its_adapter() {
    let src = r#"
class Bag {
    pub base: int,
}
impl Bag {
    #[inline(never)]
    pub fn at(int k) -> Option<int> {
        return Some(self.base + k);
    }
}
#[inline(never)]
fn through(int -> Option<int> f, int x) -> int {
    return match f(x) {
        Some(v) => v,
        None => -1,
    };
}
fn main() {
    let b = new Bag(1);
    let p = b.at();
    let _ = through(p, 2) + match b.at(3) {
        Some(v) => v,
        None => 0,
    };
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| ["Bag::at", "through", "main"].contains(&b.func.as_str())).collect();
    assert!(mine.is_empty(), "{mine:?}");
    assert!(boxes.iter().any(|b| b.func == "Bag::at$value"), "{boxes:?}");
}

/// A named function used as a value keeps its two-word result when bodies
/// emitted before it (a derived `Show`, a caller) ask how it returns.
#[test]
fn box_capture_named_fn_value_after_earlier_bodies_boxes_only_in_its_adapter() {
    let src = r#"
enum Sign {
    Neg(int),
    Zero,
    Pos(int),
}
fn first(int x) -> int {
    return match half(x) {
        Some(v) => v,
        None => 0,
    };
}
fn half(int x) -> Option<int> {
    if x % 2 != 0 {
        return None;
    }
    return Some(x / 2);
}
fn sign(int x) -> Sign {
    if x < 0 {
        return Sign::Neg(-x);
    }
    if x == 0 {
        return Sign::Zero;
    }
    return Sign::Pos(x);
}
#[inline(never)]
fn through(int -> Option<int> f, int x) -> int {
    return match f(x) {
        Some(v) => v,
        None => -1,
    };
}
fn main() {
    let s = sign;
    let k = match s(2) {
        Sign::Pos(v) => v,
        default => 0,
    };
    let _ = through(half, 4) + first(6) + k;
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| ["half", "first", "sign", "through", "main"].contains(&b.func.as_str())).collect();
    assert!(mine.is_empty(), "{mine:?}");
    assert!(boxes.iter().any(|b| b.func == "half$value"), "{boxes:?}");
}

/// A two-word local rebound by `=`, in a loop or not, keeps its two words:
/// neither the `let` nor an assignment boxes it.
#[test]
fn box_capture_rebound_pair_locals_have_no_boxes() {
    let src = r#"
#[inline(never)]
fn half(int x) -> Option<int> {
    if x % 2 == 0 {
        return Some(x / 2);
    }
    return None;
}
fn last_half(int n) -> int {
    let best = half(1);
    let i = 0;
    while i < n {
        let h = half(i);
        if h != None {
            best = h;
        }
        i = i + 1;
    }
    return match best {
        Some(v) => v,
        None => -1,
    };
}
fn fib(int n) -> int {
    let p = (0, 1);
    let i = 0;
    while i < n {
        p = (p[1], p[0] + p[1]);
        i = i + 1;
    }
    return p[0];
}
fn main() {
    let _ = last_half(9) + fib(10);
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| b.func == "last_half" || b.func == "fib").collect();
    assert!(mine.is_empty(), "{mine:?}");
}

/// A class field of a two-word type with no pointer word sits flat in the
/// object (its slot and a tail slot): `new`, a read and a write box nothing.
#[test]
fn box_capture_flat_pair_fields_have_no_boxes() {
    let src = r#"
class H {
    pub o: Option<int>,
    pub p: (int, float),
    pub n: int,
}
#[inline(never)]
fn half(int x) -> Option<int> {
    if x % 2 == 0 {
        return Some(x / 2);
    }
    return None;
}
fn run(int n) -> int {
    let h = new H(half(n), (n, 0.5), n);
    let s = 0;
    for i in 0..n {
        h.o = half(i);
        h.p = (h.p[0] + 1, h.p[1]);
        s += match h.o {
            Some(v) => v,
            None => h.p[0],
        };
    }
    return s + h.n;
}
fn main() {
    let _ = run(10);
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| b.func == "run").collect();
    assert!(mine.is_empty(), "{mine:?}");
}

#[test]
fn box_capture_generic_class_closed_pair_fields_have_no_boxes() {
    // `o` names no class parameter, so it is flat in every `G<T>`: the
    // method clone and the shared generic `reset` both store two words.
    let src = r#"
class G<T> {
    pub v: T,
    pub o: Option<int>,
}
impl G<T> {
    pub fn step(int i) {
        self.o = Some(i);
    }
}
fn reset<T>(G<T> g) {
    g.o = None;
}
fn main() {
    let g = new G("x", None);
    g.step(3);
    let f = reset;
    f(g);
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
        .filter(|b| b.func == "main" || b.func.contains("step") || b.func.contains("reset"))
        .collect();
    assert!(mine.is_empty(), "{mine:?}");
}

#[test]
fn flat_pair_field_with_a_pointer_payload_is_tagged() {
    // `r`'s payload word is a string only under `Err`, `t`'s only under
    // `Word`: each is kinded by its tail slot's tag, not left unknown.
    let src = r#"
enum Tok {
    Num(int),
    Word(string),
    End,
}
class P {
    pub r: Result<int, string>,
    pub t: Tok,
    pub n: int,
}
fn main() {
    let p = new P(Ok(1), Tok::End, 2);
    p.t = Tok::Word("w");
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    pipeline.compile_src(src).expect("compiles");
    let rows = pipeline.class_word_kinds();
    let row = rows
        .iter()
        .find(|r| r.kinds.len() == 5 + 2 * common::TAGGED_ENTRY_BYTES)
        .unwrap_or_else(|| panic!("no tagged row for P: {rows:?}"));
    let k = &row.kinds;
    assert_eq!(&k[..5], [common::WORD_TAGGED, common::WORD_TAGGED, common::WORD_SCALAR, common::WORD_SCALAR, common::WORD_SCALAR]);
    let entry = |i: usize| {
        let e = &k[5 + i * common::TAGGED_ENTRY_BYTES..][..common::TAGGED_ENTRY_BYTES];
        (u16::from_le_bytes([e[0], e[1]]), u64::from_le_bytes(e[2..10].try_into().unwrap()))
    };
    // `Err` is tag 1, `Word` tag 1: their tags sit in tail slots 3 and 4.
    assert_eq!(entry(0), (3, 0b10));
    assert_eq!(entry(1), (4, 0b10));
}

#[test]
fn box_capture_pair_captures_have_no_boxes() {
    // `o` and `r` are two-slot locals the lambda takes as their two
    // words: neither the enclosing body nor the lambda boxes them.
    let src = r#"
#[inline(never)]
fn half(int x) -> Option<int> {
    if x % 2 == 0 {
        return Some(x / 2);
    }
    return None;
}
fn make(int n) -> fn(int) -> int {
    let o = half(n);
    let r: Result<int, string> = if n > 0 { Ok(n) } else { Err("neg") };
    return fn (int k) use (o, r) => match o {
        Some(v) => v + k + match r {
            Ok(m) => m,
            Err(_) => 0,
        },
        None => k,
    };
}
fn main() {
    let _ = make(4)(1);
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| b.func.starts_with("make")).collect();
    assert!(mine.is_empty(), "{mine:?}");
}

#[test]
fn box_capture_pair_statics_have_no_boxes() {
    // `best` keeps its two words in its slot and a tail slot: neither the
    // store, the read nor its initializer boxes.
    let src = r#"
#[inline(never)]
fn half(int x) -> Option<int> {
    if x % 2 == 0 {
        return Some(x / 2);
    }
    return None;
}
static let best: Option<int> = Option::None;
fn keep(int i) -> int {
    best = half(i);
    return match best {
        Some(v) => v,
        None => -1,
    };
}
fn main() {
    let _ = keep(4);
}
"#;
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    pipeline.set_runs_mir();
    compiler::start_box_capture();
    pipeline.compile_src(src).expect("compiles");
    let boxes = compiler::take_box_capture();
    let mine: Vec<_> = boxes.iter().filter(|b| b.func == "keep" || b.func.contains("best")).collect();
    assert!(mine.is_empty(), "{mine:?}");
}
