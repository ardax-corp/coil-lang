//! Summaries of small programs: higher-order calls take the effects of the
//! functions they are passed.

use super::*;
use crate::hir::build_module;

fn summaries(src: &str) -> (HirModule, Vec<Summary>) {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    assert!(
        checker
            .messages()
            .iter()
            .all(|m| *m.kind() != reporting::MessageKind::ERROR),
        "{:?}",
        checker.messages()
    );
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let out = analyze(&module, &checker, "", &ProgramEffects::default());
    (module, out)
}

fn of(module: &HirModule, out: &[Summary], name: &str) -> Summary {
    let i = module
        .bodies
        .iter()
        .position(|b| b.name == name)
        .unwrap_or_else(|| panic!("no body `{name}`"));
    out[i]
}

const MAP: &str = "
fn map<T, U>(Vec<T> xs, T -> U f) -> Vec<U> {
    let out: Vec<U> = Vec::new();
    let i = 0;
    while i < len(xs) {
        out.push(f(xs[i]));
        i = i + 1;
    }
    return out;
}
static let HITS: int = 0;
";

#[test]
fn map_calls_its_function_parameter_and_nothing_else() {
    let (m, out) = summaries(MAP);
    let s = of(&m, &out, "map");
    assert!(s.flags.is_pure(), "{s:?}");
    assert_eq!(s.latent, 1 << 1);
    assert!(!s.is_pure());
}

#[test]
fn map_with_a_pure_lambda_is_pure() {
    let src = format!(
        "{MAP}
fn double(Vec<int> xs) -> Vec<int> {{
    return map(xs, fn (int x) => x * 2);
}}"
    );
    let (m, out) = summaries(&src);
    assert!(of(&m, &out, "double").is_pure());
}

#[test]
fn map_with_an_impure_lambda_is_impure() {
    let src = format!(
        "{MAP}
fn bump(int x) -> int {{
    HITS = HITS + 1;
    return x;
}}
fn count(Vec<int> xs) -> Vec<int> {{
    return map(xs, fn (int x) => bump(x));
}}"
    );
    let (m, out) = summaries(&src);
    let s = of(&m, &out, "count");
    assert!(s.flags.contains(EffectFlags::HEAP_MUT), "{s:?}");
}

#[test]
fn a_named_function_or_a_let_bound_lambda_passed_along_is_known() {
    let src = format!(
        "{MAP}
fn twice(int x) -> int {{
    return x * 2;
}}
fn by_name(Vec<int> xs) -> Vec<int> {{
    return map(xs, twice);
}}
fn by_let(Vec<int> xs) -> Vec<int> {{
    let g = fn (int x) => x + 1;
    return map(xs, g);
}}"
    );
    let (m, out) = summaries(&src);
    assert!(of(&m, &out, "by_name").is_pure());
    assert!(of(&m, &out, "by_let").is_pure());
}

#[test]
fn passing_a_parameter_on_keeps_it_latent() {
    let src = format!(
        "{MAP}
fn each(int n, Vec<int> xs, int -> int g) -> Vec<int> {{
    return map(xs, g);
}}
fn use_each(Vec<int> xs) -> Vec<int> {{
    return each(1, xs, fn (int x) => x);
}}"
    );
    let (m, out) = summaries(&src);
    let s = of(&m, &out, "each");
    assert!(s.flags.is_pure());
    assert_eq!(s.latent, 1 << 2);
    assert!(of(&m, &out, "use_each").is_pure());
}

#[test]
fn a_function_value_from_anywhere_else_is_unknown() {
    let src = "
fn pick(bool b) -> int -> int {
    return fn (int x) => x;
}
fn call_it(int x) -> int {
    let f = pick(true);
    return f(x);
}";
    let (m, out) = summaries(src);
    assert!(of(&m, &out, "call_it").flags.contains(EffectFlags::UNKNOWN));
}

#[test]
fn growing_a_fresh_vec_is_pure_but_a_parameter_is_not() {
    let src = "
fn fresh() -> Vec<int> {
    let v: Vec<int> = Vec::new();
    v.push(1);
    return v;
}
fn grow(Vec<int> v) {
    v.push(1);
}
fn leak(Vec<Vec<int>> all) {
    let v: Vec<int> = Vec::new();
    all.push(v);
    v.push(1);
}";
    let (m, out) = summaries(src);
    assert!(of(&m, &out, "fresh").is_pure());
    assert!(of(&m, &out, "grow").flags.contains(EffectFlags::HEAP_MUT));
    assert!(of(&m, &out, "leak").flags.contains(EffectFlags::HEAP_MUT));
}

#[test]
fn pure_names_list_pure_functions_only() {
    let src = format!(
        "{MAP}
fn double(Vec<int> xs) -> Vec<int> {{
    return map(xs, fn (int x) => x * 2);
}}
fn bump(int x) -> int {{
    HITS = HITS + 1;
    return x;
}}"
    );
    let (m, out) = summaries(&src);
    let names = pure_names(&m, "", &out, &HashSet::new());
    assert!(names.contains("double"), "{names:?}");
    assert!(!names.contains("map"));
    assert!(!names.contains("bump"));
}
