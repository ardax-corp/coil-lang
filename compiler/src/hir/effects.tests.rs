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
    let names = pure_names(&m, "", &out);
    assert!(names.contains("double"), "{names:?}");
    assert!(!names.contains("map"));
    assert!(!names.contains("bump"));
}

/// A contract check that may fail does not make a function impure; a
/// `panic` it writes itself does, and so does an effect in a clause.
#[test]
fn contract_checks_keep_a_function_pure() {
    let src = "
static let HITS: int = 0;
fn bump(int x) -> bool {
    HITS = HITS + 1;
    return x > 0;
}
fn half(int n) -> int
    requires n >= 0
    ensures result <= n
{
    return n / 2;
}
fn quarter(int n) -> int {
    return half(half(n));
}
fn count(int n) -> int {
    let i = 0;
    while i < n
        invariant i >= 0
    {
        i = i + 1;
    }
    return i;
}
fn strict(int n) -> int {
    if n < 0 {
        panic(\"negative\");
    }
    return n;
}
fn noisy(int n) -> int
    requires bump(n)
{
    return n;
}";
    let (m, out) = summaries(src);
    let names = pure_names(&m, "", &out);
    for name in ["half", "quarter", "count"] {
        assert!(names.contains(name), "`{name}`: {names:?}");
    }
    assert!(!names.contains("strict"), "{names:?}");
    assert!(!names.contains("noisy"), "{names:?}");
}

/// An operator that is not one of the scalar lanes is still built in when
/// both operands are: element-wise arithmetic, `==` on an `Option`. On a
/// class or a type parameter it runs a trait impl.
#[test]
fn operators_on_built_in_operands_are_pure() {
    let src = "
class Money {
    pub cents: int,
}
impl Lt for Money {
    pub fn lt(Money a, Money b) -> bool { return a.cents < b.cents; }
}
fn sum2([int; 2] a, [int; 2] b) -> [int; 2] { return a + b; }
fn scaled(Vec<float> xs, float k) -> Vec<float> { return xs * k; }
fn empty(Option<int> o) -> bool { return o == Option::None; }
fn same((int, string) a, (int, string) b) -> bool { return a == b; }
fn less(Money a, Money b) -> bool { return a < b; }
fn less_of<T: Lt>(T a, T b) -> bool { return a < b; }
fn has(Option<Money> o, Money m) -> bool { return o == Option::Some(m); }";
    let (m, out) = summaries(src);
    let names = pure_names(&m, "", &out);
    for name in ["sum2", "scaled", "empty", "same"] {
        assert!(names.contains(name), "`{name}`: {names:?}");
    }
    for name in ["less", "less_of", "has"] {
        assert!(!names.contains(name), "`{name}`: {names:?}");
    }
}

/// A trait impl's method is listed under its own name, and no method
/// under its short name alone: a bare `m(..)` is a call of a function, or
/// of whatever impl a trait call picks (#2436).
#[test]
fn pure_names_list_trait_impl_methods_by_their_own_name() {
    let src = "
static let HITS: int = 0;
class P {
    pub x: int,
}
impl P {
    pub fn size() -> int { return self.x; }
}
trait Weigh<T> {
    fn weigh(T x) -> int;
    fn size(T x) -> int;
}
impl Weigh for P {
    fn weigh(P p) -> int { return p.x * 2; }
    fn size(P p) -> int {
        HITS = HITS + 1;
        return p.x;
    }
}";
    let (m, out) = summaries(src);
    let names = pure_names(&m, "", &out);
    assert!(names.contains("Weigh for P::weigh"), "{names:?}");
    assert!(!names.contains("Weigh for P::size"), "{names:?}");
    assert!(names.contains("P::size"), "{names:?}");
    assert!(!names.contains("size") && !names.contains("weigh"), "{names:?}");
    let steady = steady_names(&m, "", &out);
    assert!(steady.contains("Weigh for P::weigh") && steady.contains("Weigh for P::size"), "{steady:?}");
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
    return a.area();
}
fn other(Plain p) -> int {
    return p.plain();
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

const COUNTER: &str = "
class Counter {
    pub n: int,
}
impl Counter {
    pub fn bump() {
        self.n = self.n + 1;
    }
    pub fn twice() {
        self.bump();
        self.bump();
    }
}
static let G: int = 0;
";

#[test]
fn a_method_that_writes_self_mutates_only_through_it() {
    let (m, out) = summaries(COUNTER);
    for name in ["Counter::bump", "Counter::twice"] {
        let s = of(&m, &out, name);
        assert!(s.flags.contains(EffectFlags::HEAP_MUT), "{name}: {s:?}");
        assert_eq!(s.param_only & EffectFlags::HEAP_MUT, EffectFlags::HEAP_MUT, "{name}: {s:?}");
    }
}

#[test]
fn calling_it_on_an_object_the_caller_made_is_pure() {
    let src = format!("{COUNTER}\nfn made() -> int {{ let c = new Counter(0); c.twice(); return c.n; }}");
    let (m, out) = summaries(&src);
    let s = of(&m, &out, "made");
    assert!(s.is_pure(), "{s:?}");
}

#[test]
fn calling_it_on_an_object_that_escapes_is_not_pure() {
    let src = format!(
        "{COUNTER}
fn returned() -> Counter {{ let c = new Counter(0); c.bump(); return c; }}
fn on_argument(Counter c) {{ c.bump(); }}
fn aliased() -> int {{ let c = new Counter(0); let d = c; c.bump(); return d.n; }}"
    );
    let (m, out) = summaries(&src);
    for name in ["returned", "on_argument", "aliased"] {
        let s = of(&m, &out, name);
        assert!(s.flags.contains(EffectFlags::HEAP_MUT), "{name}: {s:?}");
    }
}

#[test]
fn a_method_that_also_writes_a_static_is_not_discharged() {
    let src = format!(
        "{COUNTER}
impl Counter {{
    pub fn count() {{
        self.n = self.n + 1;
        G = G + 1;
    }}
}}
fn made() -> int {{ let c = new Counter(0); c.count(); return c.n; }}"
    );
    let (m, out) = summaries(&src);
    let s = of(&m, &out, "Counter::count");
    assert_eq!(s.param_only & EffectFlags::HEAP_MUT, 0, "{s:?}");
    assert!(of(&m, &out, "made").flags.contains(EffectFlags::HEAP_MUT));
}

fn readonly_errors(src: &str) -> Vec<String> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let program = ProgramEffects::default();
    ModuleEffects::solve(&module, &checker, "", &program)
        .readonly_messages()
        .iter()
        .map(|m| m.message().to_string())
        .collect()
}

const RESET: &str = "
trait Reset<R> {
    fn reset(R self) {}
}
class P {
    pub x: int,
}
impl Reset for P {
    fn reset(P self) {
        self.x = 0;
    }
}
";

#[test]
fn arguments_packed_into_a_rest_parameter_take_its_mask_bit() {
    let src = "
class P {
    pub x: int,
}
fn all(int a, P... rest) {
    for r in rest {
        r.x = 0;
    }
}
fn two(P x, P y) {
    all(0, x, y);
}
fn spread(int k, Vec<P> v) {
    all(k, ...v);
}
";
    let (m, out) = summaries(src);
    assert_eq!(of(&m, &out, "all").mutates, 1 << 1);
    assert_eq!(of(&m, &out, "two").mutates, 0b11, "both `x` and `y` go to `rest`");
    assert_eq!(of(&m, &out, "spread").mutates, 1 << 1, "only `v` goes to `rest`");
}

#[test]
fn a_readonly_value_spread_into_a_rest_parameter_is_e0107() {
    let src = format!(
        "{RESET}
fn all<A, B: Reset>(A a, B... rest) {{
    for r in rest {{
        r.reset();
    }}
}}
fn main() {{
    let p = readonly new P(1);
    let v: Vec<readonly P> = Vec::new();
    v.push(p);
    all(0, ...v);
    all(0, new P(2), p);
}}
"
    );
    let errs = readonly_errors(&src);
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert!(errs.iter().all(|e| e.contains("parameter `rest`")), "{errs:?}");
}

#[test]
fn a_readonly_method_argument_is_named_not_called_on() {
    let src = format!(
        "{RESET}
class H {{
    pub n: int,
}}
impl H {{
    pub fn put<B: Reset>(int k, B b) {{
        b.reset();
    }}
}}
fn main() {{
    let p = readonly new P(1);
    let h = new H(0);
    h.put(0, p);
}}
"
    );
    let errs = readonly_errors(&src);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].starts_with("Cannot pass the `readonly` `p` to `.put()`: it mutates parameter `b`"), "{errs:?}");
}

// Escape facts (`escapes`, `returns`, `shared_ret`; feature/escape-memory step 1).

const TREES: &str = "
enum Tree {
    Leaf,
    Node(Tree, Tree),
}
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
";

#[test]
fn a_tree_built_and_checked_stays_with_the_caller() {
    let (m, out) = summaries(TREES);
    let build = of(&m, &out, "bottom_up");
    assert!(!build.shared_ret, "{build:?}");
    let check = of(&m, &out, "item_check");
    assert_eq!(check.escapes, 0, "{check:?}");
    assert_eq!(check.returns, 0, "{check:?}");
}

#[test]
fn a_store_into_a_static_or_a_parameter_keeps_the_value() {
    let src = "
class Box2 { pub v: string }
static let LOG: Vec<string> = Vec::new();
fn remember(string s) {
    LOG.push(s);
}
fn put(Box2 b, string s) {
    b.v = s;
}
";
    let (m, out) = summaries(src);
    assert_eq!(of(&m, &out, "remember").escapes, 1);
    let put = of(&m, &out, "put");
    assert_eq!(put.escapes, 1 << 1, "{put:?}");
}

#[test]
fn a_getter_hands_back_what_its_receiver_reaches() {
    let src = "
class Holder {
    pub name: string,
}
impl Holder {
    pub fn get() -> string {
        return self.name;
    }
}
";
    let (m, out) = summaries(src);
    let get = of(&m, &out, "Holder::get");
    assert_eq!(get.escapes, 0, "{get:?}");
    assert_eq!(get.returns, 1, "{get:?}");
    assert!(get.shared_ret);
}

#[test]
fn a_new_vec_holding_a_parameter_returns_it() {
    let src = "
fn wrap(string s) -> Vec<string> {
    let out: Vec<string> = Vec::new();
    out.push(s);
    return out;
}
fn fresh_vec(int n) -> Vec<int> {
    let out: Vec<int> = Vec::new();
    out.push(n);
    return out;
}
";
    let (m, out) = summaries(src);
    let wrap = of(&m, &out, "wrap");
    assert_eq!(wrap.escapes, 0, "{wrap:?}");
    assert_eq!(wrap.returns, 1, "{wrap:?}");
    assert!(wrap.shared_ret);
    let fresh = of(&m, &out, "fresh_vec");
    assert!(!fresh.shared_ret, "{fresh:?}");
}

#[test]
fn a_captured_parameter_escapes_and_a_literal_counts_as_fresh() {
    let src = "
fn later(string s) -> () -> int {
    return fn () use (s) => s.len();
}
fn name() -> string {
    return \"coil\";
}
";
    let (m, out) = summaries(src);
    assert_eq!(of(&m, &out, "later").escapes, 1);
    // Interned and immutable: holding one is like holding nothing.
    assert!(!of(&m, &out, "name").shared_ret);
}

#[test]
fn a_local_byte_copy_keeps_nothing() {
    let src = "
use string::{to_bytes};
fn count_commas(string s) -> int {
    let b = to_bytes(s);
    // Changed, so it stays a copy.
    b.push(\".\" as byte);
    let n = 0;
    let i = 0;
    while i < b.len() {
        if b[i] == \",\" {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}
";
    let (m, out) = summaries(src);
    let s = of(&m, &out, "count_commas");
    assert_eq!(s.escapes, 0, "{s:?}");
}

#[test]
fn an_unknown_function_value_keeps_its_arguments() {
    let src = "
fn give(string s, string -> int f) -> int {
    return f(s);
}
";
    let (m, out) = summaries(src);
    assert_eq!(of(&m, &out, "give").escapes & 1, 1);
}

#[test]
fn a_fresh_object_given_to_a_mutating_callee_comes_back_shared() {
    let src = "
class Holder {
    pub items: Vec<string>,
}
fn add(Holder h, string s) {
    h.items.push(s);
}
fn build(string s) -> Holder {
    let h = new Holder(Vec::new());
    add(h, s);
    return h;
}
";
    let (m, out) = summaries(src);
    // `add` may have stored anything into `h`: freeing what `build`
    // returns could free `s`.
    assert!(of(&m, &out, "build").shared_ret);
}

#[test]
fn a_partial_application_keeps_its_arguments() {
    let src = "
class Box {
    pub n: int,
}
fn weigh(Box b, int k) -> int {
    return b.n * k;
}
fn bind(Box b) -> (int) -> int {
    return weigh(b);
}
";
    let (m, out) = summaries(src);
    assert_eq!(of(&m, &out, "weigh").escapes, 0);
    assert_eq!(of(&m, &out, "bind").escapes & 1, 1);
}

// Early frees (feature/escape-memory step 2).

/// The body `name` after the early-free rewrite, if it changed.
fn rewritten(src: &str, name: &str) -> Option<HirBody> {
    rewritten_with(src, name, &Slots { local: &|_| false, value: &|_| false })
}

/// [`rewritten`], with what lowering keeps in frame slots.
fn rewritten_with(src: &str, name: &str, slots: &Slots<'_>) -> Option<HirBody> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let mut program = ProgramEffects::default();
    let out = ModuleEffects::solve(&module, &checker, "", &program).summaries;
    program.record(&module, &checker, "", &out);
    let body = module.bodies.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("no body `{name}`"));
    let host = |id: HirId| match &body.expr(id).kind {
        HirKind::Call { callee: Callee::Named { name, .. }, .. } => common::host_native_id(name),
        _ => None,
    };
    program.early_frees(&checker, "", &module, body, &host, slots)
}

/// The names of the locals `body` frees early.
fn freed(body: &HirBody) -> Vec<String> {
    body.exprs
        .iter()
        .filter_map(|x| match &x.kind {
            HirKind::Builtin { op: crate::hir::Builtin::FreeEarly { .. }, args } => match body.expr(args[0]).kind {
                HirKind::Local(l) => Some(body.local(l).name.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Names of the calls left in `body`, and whether each takes its buffer.
fn calls(body: &HirBody) -> Vec<(String, bool)> {
    body.exprs
        .iter()
        .filter_map(|x| match &x.kind {
            HirKind::Call { callee: Callee::Named { name, .. }, .. } => Some((name.clone(), x.flags.contains(crate::hir::HirFlags::TAKE))),
            _ => None,
        })
        .collect()
}

/// `(deep, local name)` of each early free the rewrite adds to `name`.
fn frees(src: &str, name: &str) -> Vec<(bool, String)> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let mut program = ProgramEffects::default();
    let out = ModuleEffects::solve(&module, &checker, "", &program).summaries;
    program.record(&module, &checker, "", &out);
    let body = module.bodies.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("no body `{name}`"));
    // Host natives by name, as the planner finds them.
    let host = |id: HirId| match &body.expr(id).kind {
        HirKind::Call { callee: Callee::Named { name, .. }, .. } => common::host_native_id(name),
        _ => None,
    };
    let Some(rewritten) = program.early_frees(&checker, "", &module, body, &host, &Slots { local: &|_| false, value: &|_| false }) else {
        return Vec::new();
    };
    rewritten
        .exprs
        .iter()
        .filter_map(|x| match &x.kind {
            HirKind::Builtin { op: crate::hir::Builtin::FreeEarly { deep }, args } => match rewritten.expr(args[0]).kind {
                HirKind::Local(l) => Some((*deep, rewritten.local(l).name.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn a_tree_checked_right_after_it_is_built_is_freed_whole() {
    let src = format!("{TREES}\nfn run(int d) -> int {{\n    return item_check(bottom_up(d));\n}}\n");
    let got = frees(&src, "run");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0, "{got:?}");
}

#[test]
fn a_local_byte_copy_is_freed_before_the_return() {
    let src = "
use string::{to_bytes};
fn count_commas(string s) -> int {
    let b = to_bytes(s);
    // Changed, so it stays a copy.
    b.push(\".\" as byte);
    let n = 0;
    let i = 0;
    while i < b.len() {
        if b[i] == \",\" {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}
";
    assert_eq!(frees(src, "count_commas"), vec![(false, "b".to_string())]);
}

#[test]
fn a_stored_returned_or_aliased_object_is_not_freed() {
    let src = "
use string::{to_bytes};
fn keep(Vec<string> out, string s) {
    let t = s + \"!\";
    out.push(t);
}
fn give(string s) -> string {
    let t = s + \"!\";
    return t;
}
fn alias(string s) -> int {
    let a = to_bytes(s);
    let b = a;
    return b.len();
}
";
    assert!(frees(src, "keep").is_empty());
    assert!(frees(src, "give").is_empty());
    assert!(frees(src, "alias").is_empty());
}

#[test]
fn a_local_byte_copy_is_freed_before_every_return() {
    let src = "
use string::{to_bytes};
fn needs_quote(string s) -> bool {
    let b = to_bytes(s);
    // Changed, so it stays a copy.
    b.push(\".\" as byte);
    let i = 0;
    while i < b.len() {
        if b[i] == \",\" {
            return true;
        }
        i = i + 1;
    }
    return false;
}
";
    assert_eq!(frees(src, "needs_quote"), vec![(false, "b".to_string()), (false, "b".to_string())]);
}

#[test]
fn a_string_rebuilt_in_place_frees_its_old_value() {
    let src = "
fn build(int n) -> string {
    let s = \"\";
    let i = 0;
    while i < n {
        s = s + \"ab\";
        i = i + 1;
    }
    return s;
}
";
    assert_eq!(frees(src, "build"), vec![(false, "s".to_string())]);
}

#[test]
fn a_string_copied_before_a_rebuild_is_not_freed() {
    let src = "
fn snapshot(int n) -> string {
    let s = \"x\";
    let keep = s;
    let i = 0;
    while i < n {
        s = s + \"y\";
        i = i + 1;
    }
    return keep + s;
}
";
    assert_eq!(frees(src, "snapshot"), vec![]);
}

#[test]
fn a_fresh_concat_operand_is_freed_after_the_concat() {
    let src = "
use string::{format};
fn piece(int i) -> string {
    return format(\"<%i>\", i);
}
fn wrap(int i) -> string {
    return \"[\" + piece(i) + \"]\";
}
";
    assert_eq!(frees(src, "wrap").len(), 1);
}


#[test]
fn a_byte_buffer_a_method_appends_to_is_still_freed() {
    let src = "
use string::{from_bytes};
class P {
    pub i: int,
}
impl P {
    pub fn esc(Vec<byte> out) {
        out.push(\"x\" as byte);
        self.i = self.i + 1;
    }
    pub fn parse() -> Result<string, int> {
        let out: Vec<byte> = Vec::new();
        while self.i < 10 {
            if self.i == 3 {
                self.esc(out);
                continue;
            }
            out.push(\"a\" as byte);
            self.i = self.i + 1;
        }
        return match from_bytes(out) {
            Result::Ok(s) => s,
            Result::Err(_) => raise 1,
        };
    }
}
";
    // Before the `raise` and before the final `return`.
    assert_eq!(frees(src, "P::parse"), vec![(false, "out".to_string()), (false, "out".to_string())]);
}

const NODES: &str = "
class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn step(int i) -> Result<Node, Node> {
    if i % 2 == 0 {
        return Result::Ok(new Node(i, Option::None));
    }
    return Result::Err(new Node(i, Option::None));
}

fn build(int n) -> Option<Node> {
    let head: Option<Node> = Option::None;
    let i = 0;
    while i < n {
        head = Option::Some(new Node(i, head));
        i = i + 1;
    }
    return head;
}

fn len_of(Option<Node> head) -> int {
    let n = 0;
    let cur = head;
    let go = true;
    while go {
        match cur {
            Option::Some(node) => {
                n = n + 1;
                cur = node.next;
            },
            Option::None => {
                go = false;
            },
        }
    }
    return n;
}
";

const LINES: &str = "
fn line(string a, int i) -> Option<string> {
    if i % 2 == 0 {
        return Option::Some(a + \"cd\");
    }
    return Option::None;
}
";

#[test]
fn a_fresh_match_scrutinee_read_in_place_is_freed_after_the_match() {
    let src = format!("{LINES}\nfn run(int i) -> int {{\n    return match line(\"ab\", i) {{\n        Option::Some(s) => s[0] as int,\n        Option::None => 0,\n    }};\n}}\n");
    assert_eq!(frees(&src, "run").len(), 1, "{:?}", frees(&src, "run"));
}

#[test]
fn a_match_binding_kept_past_the_match_is_not_freed() {
    let src = format!("{LINES}\nfn run(int i) -> string {{\n    return match line(\"ab\", i) {{\n        Option::Some(s) => s,\n        Option::None => \"\",\n    }};\n}}\n");
    assert_eq!(frees(&src, "run"), vec![]);
}

#[test]
fn a_small_fixed_size_match_scrutinee_is_left_to_the_collector() {
    let src = format!("{NODES}\nfn run(int i) -> int {{\n    return match step(i) {{\n        Result::Ok(n) => n.v,\n        Result::Err(e) => e.v,\n    }};\n}}\n");
    assert_eq!(frees(&src, "run"), vec![]);
}

#[test]
fn a_list_replaced_by_reassignment_frees_the_old_list_whole() {
    let src = format!("{NODES}\nfn run(int k) -> int {{\n    let total = 0;\n    let list = build(1);\n    let r = 0;\n    while r < k {{\n        list = build(100);\n        total = total + len_of(list);\n        r = r + 1;\n    }}\n    return total;\n}}\n");
    assert!(frees(&src, "run").contains(&(true, "list".to_string())), "{:?}", frees(&src, "run"));
}

#[test]
fn a_list_another_local_shares_is_not_freed_on_reassignment() {
    let src = format!("{NODES}\nfn run(int k) -> int {{\n    let list = build(1);\n    let first = list;\n    let r = 0;\n    while r < k {{\n        list = build(100);\n        r = r + 1;\n    }}\n    return len_of(first);\n}}\n");
    assert!(!frees(&src, "run").iter().any(|f| f.1 == "list"), "{:?}", frees(&src, "run"));
}

#[test]
fn a_byte_copy_only_indexed_reads_the_string_instead() {
    let src = "
use string::{to_bytes};
fn commas(string s) -> int {
    let b = to_bytes(s);
    let n = 0;
    let i = 0;
    while i < b.len() {
        if b[i] == \",\" {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}
";
    let body = rewritten(src, "commas").expect("rewritten");
    // The copy's `let` binds nothing, and the reads index the string.
    assert!(body.exprs.iter().any(|x| matches!(x.kind, HirKind::Let { init: None, .. })));
    assert!(body.exprs.iter().any(|x| matches!(x.kind, HirKind::Index { kind: crate::hir::IndexKind::String, .. })));
}

#[test]
fn a_byte_copy_that_is_changed_stays_a_copy() {
    let src = "
use string::{to_bytes};
fn bump(string s) -> int {
    let b = to_bytes(s);
    b[0] = \"x\";
    return b.len();
}
";
    let body = rewritten(src, "bump");
    assert!(!body.is_some_and(|b| b.exprs.iter().any(|x| matches!(x.kind, HirKind::Index { kind: crate::hir::IndexKind::String, .. }))));
}

#[test]
fn a_buffer_returned_as_a_string_gives_up_its_bytes() {
    let src = "
use string::{from_bytes};
fn word(int n) -> string {
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < n {
        out.push(\"a\" as byte);
        i = i + 1;
    }
    return match from_bytes(out) {
        Result::Ok(s) => s,
        Result::Err(_) => \"\",
    };
}
";
    let body = rewritten(src, "word").expect("rewritten");
    assert!(calls(&body).iter().any(|c| c.0.ends_with("from_bytes") && c.1), "{:?}", calls(&body));
}

#[test]
fn a_buffer_read_after_its_conversion_keeps_its_bytes() {
    let src = "
use string::{from_bytes};
fn word(int n) -> int {
    let out: Vec<byte> = Vec::new();
    out.push(\"a\" as byte);
    return match from_bytes(out) {
        Result::Ok(s) => out.len(),
        Result::Err(_) => 0,
    };
}
";
    let left = rewritten(src, "word").map(|b| calls(&b)).unwrap_or_default();
    assert!(!left.iter().any(|c| c.1), "{left:?}");
}

#[test]
fn a_parameter_buffer_keeps_its_bytes() {
    let src = "
use string::{from_bytes};
fn text(Vec<byte> b) -> string {
    return match from_bytes(b) {
        Result::Ok(s) => s,
        Result::Err(_) => \"\",
    };
}
";
    let left = rewritten(src, "text").map(|b| calls(&b)).unwrap_or_default();
    assert!(!left.iter().any(|c| c.1), "{left:?}");
}

#[test]
fn a_returned_parameter_is_returned_not_kept() {
    // #2206: copy-on-write tells "may be the result" apart from "stored
    // past the call", so a caller can skip the share when it moves the
    // argument.
    let src = "
class Box {
    pub items: Vec<int>,
}
fn add(Vec<int> vs) -> Vec<int> {
    vs.push(1);
    return vs;
}
fn stash(Box b, Vec<int> vs) -> Vec<int> {
    b.items = vs;
    return vs;
}
";
    let (m, out) = summaries(src);
    let add = of(&m, &out, "add");
    assert_eq!(add.keeps, 0, "{add:?}");
    assert_eq!(add.returns_arg, 1, "{add:?}");
    let stash = of(&m, &out, "stash");
    assert_eq!(stash.keeps, 0b10, "{stash:?}");
    assert_eq!(stash.returns_arg, 0b10, "{stash:?}");
}

#[test]
fn a_returned_field_is_returned_inner() {
    // #2206: a parameter's field or element comes back unshared, and the
    // caller shares it where it keeps it.
    let src = "
class Box {
    pub items: Vec<int>,
}
fn items_of(int n, Box b) -> Vec<int> {
    return b.items;
}
fn fresh(Box b) -> Vec<int> {
    let v: Vec<int> = Vec::new();
    return v;
}
";
    let (m, out) = summaries(src);
    let items_of = of(&m, &out, "items_of");
    assert_eq!(items_of.returns_inner, 0b10, "{items_of:?}");
    assert_eq!(items_of.returns_arg, 0, "{items_of:?}");
    let fresh = of(&m, &out, "fresh");
    assert_eq!(fresh.returns_inner, 0, "{fresh:?}");
}

/// A copy that is changed, then a block that leaves on every path, and a
/// match on a fresh string whose arms all leave.
const LEAVERS: &str = "
use string::{to_bytes};
fn sign(string s, int n) -> int {
    let b = to_bytes(s);
    b.push(\".\" as byte);
    if n > b.len() {
        return 1;
    } else {
        return 0;
    }
}
fn shout(string s) -> Option<string> {
    if s.len() > 0 {
        return Option::Some(s + \"!\");
    }
    return Option::None;
}
fn loud(string s) -> int {
    match shout(s) {
        Option::Some(t) => {
            return t.len();
        },
        Option::None => {
            return 0;
        },
    }
}
";

#[test]
fn a_value_that_never_arrives_is_bound_to_no_temp() {
    // D4b: a `never` temp is a local no layout places, and the whole
    // rewrite was thrown away with it.
    for name in ["sign", "loud"] {
        let Some(body) = rewritten(LEAVERS, name) else { continue };
        let never: Vec<&str> = body.locals.iter().filter(|l| matches!(l.ty, Some(Ty::Never))).map(|l| l.name.as_str()).collect();
        assert!(never.is_empty(), "{name}: {never:?}");
    }
    let sign = rewritten(LEAVERS, "sign").expect("sign frees its copy");
    assert_eq!(freed(&sign), vec!["b".to_string(), "b".to_string()]);
}

#[test]
fn what_lowering_keeps_in_frame_slots_is_not_freed() {
    // D4b: a two-word local or call result and a stack array have no
    // object; a free of one did not plan.
    let src = format!(
        "{TREES}\nuse string::{{to_bytes}};\nfn run(int d) -> int {{\n    return item_check(bottom_up(d));\n}}\nfn count(string s) -> int {{\n    let b = to_bytes(s);\n    b.push(\".\" as byte);\n    return b.len();\n}}\n"
    );
    let all = Slots { local: &|_| true, value: &|_| true };
    assert_eq!(rewritten(&src, "run").map(|b| freed(&b).len()), Some(1));
    assert_eq!(rewritten_with(&src, "run", &all).map(|b| freed(&b)).unwrap_or_default(), Vec::<String>::new());
    assert_eq!(rewritten(&src, "count").map(|b| freed(&b)), Some(vec!["b".to_string()]));
    assert_eq!(rewritten_with(&src, "count", &all).map(|b| freed(&b)).unwrap_or_default(), Vec::<String>::new());
}

#[test]
fn a_call_a_field_is_read_off_stays_a_call() {
    // D4b: the read takes the call's own layout as its base, which the
    // block freeing the call's argument does not have.
    let src = "
class Rec {
    pub n: int,
    pub tag: string,
}
fn make(int n) -> Rec {
    return new Rec(n, \"r\");
}
fn later(Rec r) -> Rec {
    return make(r.n * 2);
}
fn run(int n) -> int {
    return later(make(n)).n;
}
fn kept(int n) -> int {
    let r = later(make(n));
    return r.n;
}
";
    assert!(rewritten(src, "kept").is_some_and(|b| !freed(&b).is_empty()), "a local's base is freed after the read");
    let Some(body) = rewritten(src, "run") else { return };
    for x in &body.exprs {
        if let HirKind::Field { base, .. } = x.kind {
            assert!(!matches!(body.expr(base).kind, HirKind::Block { .. }), "a block as a field base");
        }
    }
}

#[test]
fn a_value_of_literals_is_no_object_to_free() {
    // D4b: a `Some` of a literal or a `None` is no object made at run
    // time; a free of it is a call that does nothing, once per match.
    let src = "
fn tagged(int n) -> int {
    return match (if n % 3 == 0 { Option::Some(\"hit\") } else { Option::None }) {
        Option::Some(_) => 1,
        Option::None => 0,
    };
}
fn built(int n, string s) -> int {
    return match (if n % 3 == 0 { Option::Some(s + \"!\") } else { Option::None }) {
        Option::Some(t) => t.len(),
        Option::None => 0,
    };
}
";
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let scrutinee = |name: &str| {
        let body = module.bodies.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("no body `{name}`"));
        let found = body.exprs.iter().find_map(|x| match x.kind {
            HirKind::Match { scrutinee, .. } => Some(scrutinee),
            _ => None,
        });
        (body, found.expect("a match"))
    };
    let (body, v) = scrutinee("tagged");
    assert!(!early_free::allocates(body, v));
    let (body, v) = scrutinee("built");
    assert!(early_free::allocates(body, v));
}

fn drop_resizes(src: &str) -> bool {
    let (module, out) = summaries(src);
    let checker = Checker::new();
    let mut program = ProgramEffects::default();
    program.record(&module, &checker, "", &out);
    program.drop_may_resize()
}

/// A finalizer runs at any allocation: one that may change a length makes
/// every allocation a point where lengths change.
#[test]
fn a_drop_that_pops_may_resize() {
    assert!(drop_resizes(
        "
class Log {
    pub lines: Vec<int>,
}
impl Log {
    fn drop() {
        self.lines.pop();
    }
}"
    ));
}

#[test]
fn a_drop_that_runs_unknown_code_may_resize() {
    assert!(drop_resizes(
        "
class Hook {
    pub f: int -> int,
}
impl Hook {
    fn drop() {
        let g = self.f;
        let _ = g(1);
    }
}"
    ));
}

#[test]
fn a_drop_that_writes_a_field_or_a_static_leaves_lengths_alone() {
    assert!(!drop_resizes(
        "
static let DROPS: int = 0;
class Handle {
    pub fd: int,
}
impl Handle {
    fn drop() {
        DROPS = DROPS + 1;
        self.fd = 0 - 1;
    }
    pub fn grow(Vec<int> v) {
        v.push(self.fd);
    }
}"
    ));
}

const OVERLOADS: &str = "
static let HITS: int = 0;
fn f(int a, int b) -> int {
    HITS = HITS + 1;
    return a + b;
}
fn f(int a) -> int { return a + 1; }
fn one(int a) -> int { return f(a); }
fn two(int a) -> int { return f(a, a); }
class C {
    pub n: int,
}
impl C {
    pub fn m(int a, int b) -> int {
        HITS = HITS + 1;
        return a + b;
    }
    pub fn m(int a) -> int { return a + self.n; }
}
fn mone(C c) -> int { return c.m(1); }
fn mtwo(C c) -> int { return c.m(1, 2); }
";

/// A call of an overloaded function costs what the overload it names
/// does, whichever is declared last (#2426).
#[test]
fn a_call_takes_the_effects_of_the_overload_it_names() {
    let (m, out) = summaries(OVERLOADS);
    assert!(of(&m, &out, "one").is_pure());
    assert!(!of(&m, &out, "two").is_pure());
    // A method call does not say which body it runs: either may.
    assert!(!of(&m, &out, "mone").is_pure());
    assert!(!of(&m, &out, "mtwo").is_pure());
}

#[test]
fn an_overloaded_name_is_pure_when_every_overload_is() {
    let (m, out) = summaries(OVERLOADS);
    let names = pure_names(&m, "", &out);
    assert!(!names.contains("f") && !names.contains("C::m") && !names.contains("m"), "{names:?}");
    assert!(names.contains("one") && !names.contains("two"), "{names:?}");
}

#[test]
fn a_later_module_sees_every_overload_of_a_name() {
    let (m, out) = summaries(OVERLOADS);
    let mut program = ProgramEffects::default();
    program.record(&m, &Checker::new(), "", &out);
    assert!(!program.summary_named("f").expect("f").is_pure());
    assert!(!program.summary_named("C::m").expect("C::m").is_pure());
}

/// A function the program declares is never a host native, whatever its
/// name: an `extern` has no body to summarise, so its call is unknown code.
#[test]
fn an_extern_named_like_a_host_native_is_unknown_code() {
    let src = "
extern \"plugin\" {
    fn math_sin(float x) -> float;
}
fn wave(float x) -> float { return math_sin(x); }
fn plain(float x) -> float { return x * 2.0; }
";
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    // The library is not there to load; the declaration still stands.
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let program = ProgramEffects::default();
    let out = ModuleEffects::solve(&module, &checker, "", &program).summaries;
    let wave = of(&module, &out, "wave");
    assert!(wave.flags.contains(EffectFlags::UNKNOWN) && !wave.is_steady(), "{wave:?}");
    assert!(of(&module, &out, "plain").is_pure());
}

/// A function is listed under its local name and with its module's path,
/// and a call is matched by the whole name.
#[test]
fn names_carry_the_module_path_and_match_whole() {
    let (m, out) = summaries("fn sq(int x) -> int { return x * x; }");
    let mut m = m;
    m.bodies[0].name = "util::sq".to_string();
    let names = pure_names(&m, "util", &out);
    assert!(names.contains("sq") && names.contains("util::sq"), "{names:?}");
    assert!(crate::pure_call::name_in(&names, "util::sq") && crate::pure_call::name_in(&names, "sq"));
    assert!(!crate::pure_call::name_in(&names, "other::sq") && !crate::pure_call::name_in(&names, "Type::sq"));
}
