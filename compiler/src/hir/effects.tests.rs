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
    let program = ProgramEffects::default();
    let out = ModuleEffects::solve(&module, &checker, "", &program).summaries;
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
    while i < xs.len() {
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

fn violations(src: &str) -> Vec<String> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let program = ProgramEffects::default();
    ModuleEffects::solve(&module, &checker, "", &program)
        .violations()
        .into_iter()
        .map(|v| format!("{} [{}]", v.message, v.help))
        .collect()
}

#[test]
fn panics_are_not_user_visible() {
    let src = "
fn pick(Vec<int> v, int i) -> int {
    if i > 9 {
        panic(\"too far\");
    }
    return v[i];
}";
    let (m, out) = summaries(src);
    let s = of(&m, &out, "pick");
    assert!(s.visible.is_pure(), "{s:?}");
    assert!(!s.flags.is_pure(), "auto-par still sees the panic: {s:?}");
}

#[test]
fn a_broken_declaration_names_the_call_chain() {
    let src = "
static let HITS: int = 0;
fn bump() {
    HITS = HITS + 1;
}
fn step(int x) -> int {
    bump();
    return x;
}
pure fn run(int x) -> int {
    return step(x);
}
fn ok(int x) -> int uses {read, mutate} {
    return step(x);
}";
    assert_eq!(
        violations(src),
        vec![
            "`run` is declared `pure` but needs read, mutate: run → step → bump needs mutate: it writes static `HITS` \
             [declare `uses {read, mutate}`]"
                .to_string()
        ]
    );
}

#[test]
fn a_parameter_called_is_not_the_functions_own_effect() {
    let src = format!(
        "{MAP}
fn bump(int x) -> int {{
    HITS = HITS + 1;
    return x;
}}
fn each(Vec<int> xs, int -> int f) -> Vec<int> uses {{}} {{
    return map(xs, f);
}}
pure fn count(Vec<int> xs) -> Vec<int> {{
    return each(xs, bump);
}}"
    );
    let found = violations(&src);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("`count` is declared `pure` but needs read, mutate: count → bump"), "{found:?}");
}

#[test]
fn trait_declarations_bound_impls_and_calls_through_the_trait() {
    let src = "
static let HITS: int = 0;
trait Area<A> {
    pure fn area(A self) -> int;
}
trait Plain<P> {
    fn plain(P self) -> int;
}
class Sq {
    pub side: int,
}
impl Area for Sq {
    fn area(Sq self) -> int {
        HITS = HITS + 1;
        return self.side;
    }
}
impl Plain for Sq {
    fn plain(Sq self) -> int {
        return 1;
    }
}
fn total(Area a) -> int {
    return area(a);
}
fn other(Plain p) -> int {
    return plain(p);
}";
    let (m, out) = summaries(src);
    assert!(of(&m, &out, "total").visible.is_pure());
    assert!(of(&m, &out, "other").visible.contains(EffectFlags::UNKNOWN));
    let found = violations(src);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("`Area for Sq::area` implements `Area::area`, declared `pure` but needs read, mutate"),
        "{found:?}"
    );
}

#[test]
fn descriptions_use_the_uses_vocabulary() {
    let src = "
use io::stdout;
use io::sync::write_all;
use string::to_bytes;
fn say(string s) {
    write_all(stdout(), to_bytes(s));
}";
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let described = describe_fns(&checker, &ast);
    let say = described.iter().find(|(n, _)| n == "say").map(|(_, d)| d.as_str());
    assert_eq!(say, Some("uses {write}: calls `write_all` (write); calls `stdout` (write)"));
}

const MUTATORS: &str = "
class Inner {
    pub n: int,
}
class P {
    pub x: int,
    pub inner: Inner,
}
impl P {
    pub fn get() -> int {
        return self.x;
    }
    pub fn bump() {
        self.x = self.x + 1;
    }
    pub fn deep() {
        let i = self.inner;
        i.n = 0;
    }
    pub fn twice() {
        self.bump();
    }
}
fn copy_into(Vec<int> src, Vec<int> dst) {
    for x in src {
        dst.push(x);
    }
}
fn fresh(P p) -> int {
    let q = new P(1, new Inner(2));
    q.x = p.x;
    return q.x;
}
";

#[test]
fn mutates_names_each_written_parameter() {
    let (m, out) = summaries(MUTATORS);
    assert_eq!(of(&m, &out, "P::get").mutates, 0);
    assert_eq!(of(&m, &out, "P::bump").mutates, 1);
    assert_eq!(of(&m, &out, "P::deep").mutates, 1, "a write through a field read of self");
    assert_eq!(of(&m, &out, "P::twice").mutates, 1, "through a callee that mutates self");
    assert_eq!(of(&m, &out, "copy_into").mutates, 1 << 1, "only `dst`");
    assert_eq!(of(&m, &out, "fresh").mutates, 0, "a local object is not a parameter");
}

#[test]
fn descriptions_name_mutated_parameters() {
    let owned = Box::leak(MUTATORS.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let described = describe_fns(&checker, &ast);
    let get = |name: &str| described.iter().find(|(n, _)| n == name).map(|(_, d)| d.clone()).unwrap_or_default();
    assert!(get("P::bump").ends_with("; mutates self"), "{}", get("P::bump"));
    assert!(get("copy_into").ends_with("; mutates `dst`"), "{}", get("copy_into"));
    assert_eq!(get("P::get"), "pure");
}
