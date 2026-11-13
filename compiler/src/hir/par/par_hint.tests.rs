//! Which call bags get an E0805 hint, and what it names.

use super::*;
use crate::hir::build_module;

/// A parsed file, kept for the hints' spans.
type Parsed = parser::ast::Output<'static>;

/// The hints for a lone file, on the effects of its own functions.
fn hints_of(ast: &Parsed) -> Vec<ParEscapeHint> {
    let mut checker = Checker::new();
    let _ = checker.check_program(ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", ast);
    let (_, impure) = crate::hir::effects::lone_module_effects(&module, &checker);
    module.par_escape_hints(&checker, &impure)
}

fn parse(src: &'static str) -> Parsed {
    parser::Pratt::default().parse(src).expect("parse")
}

#[test]
fn unlocked_stdout_recursive_bag_is_named() {
    let ast = parse(
        r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn leaf(int n) -> int {
    write(stdout(), to_bytes(format("%i", n)));
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return leaf(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() { return; }
"#,
    );
    let hints = hints_of(&ast);
    let rec = hints.iter().find(|h| h.fn_name == "rec").expect("rec hint");
    assert!(!rec.covering_lock);
    assert!(
        rec.resources
            .iter()
            .any(|r| r.name == "stdout" && r.kind == EscapeKind::Fd),
        "named FD: {:?}",
        rec.resources
    );
    assert!(rec.message().contains("`stdout` (FD)"));
    assert_eq!(
        rec.primary_resource().map(|r| r.name.as_str()),
        Some("stdout")
    );
    assert!(hints.iter().all(|h| h.fn_name != "fib"));
}

#[test]
fn covering_with_lock_documents_gate() {
    let ast = parse(
        r#"
use thread::{mutex, with_lock};
fn rec(Mutex m, int n) -> int {
    if n <= 1 { return 1; }
    return with_lock(m, fn (int x) => (rec(m, n - 1) + rec(m, n - 2), 0))?;
}
fn main() { return; }
"#,
    );
    let hints = hints_of(&ast);
    let rec = hints.iter().find(|h| h.fn_name == "rec").expect("rec hint");
    assert!(rec.covering_lock, "{hints:?}");
    assert!(
        rec.resources
            .iter()
            .any(|r| r.name == "m" && r.kind == EscapeKind::Mutex),
        "{:?}",
        rec.resources
    );
    assert!(rec.message().contains("covering lock on `m` (mutex)"));
    assert!(rec.message().contains("lockable edges"));
}

#[test]
fn open_bound_local_is_named_on_write() {
    let ast = parse(
        r#"
fn dump(int n) -> int {
    let f = open("x", "w")?;
    write(f, n);
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return dump(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() { return; }
"#,
    );
    let rec = hints_of(&ast)
        .into_iter()
        .find(|h| h.fn_name == "rec")
        .expect("rec hint");
    assert!(
        rec.resources
            .iter()
            .any(|r| r.name == "f" && r.kind == EscapeKind::Fd),
        "open-bound local: {:?}",
        rec.resources
    );
    assert!(rec.message().contains("`f` (FD)"));
}

#[test]
fn stderr_and_stdout_are_both_listed() {
    let ast = parse(
        r#"
fn leaf(int n) -> int {
    write(stdout(), n);
    write(stderr(), n);
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return leaf(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() { return; }
"#,
    );
    let rec = hints_of(&ast)
        .into_iter()
        .find(|h| h.fn_name == "rec")
        .expect("rec hint");
    let msg = rec.message();
    assert!(
        msg.contains("`stderr` (FD)") && msg.contains("`stdout` (FD)"),
        "multi-edge hint: {msg} resources={:?}",
        rec.resources
    );
    assert!(msg.contains(", "));
}

#[test]
fn ffi_invoke_and_dload_are_named() {
    let ast = parse(
        r#"
fn poke(int n) -> int {
    let lib = dload("plugin")?;
    invoke(lib, 0, n);
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return poke(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() { return; }
"#,
    );
    let rec = hints_of(&ast)
        .into_iter()
        .find(|h| h.fn_name == "rec")
        .expect("rec hint");
    let msg = rec.message();
    assert!(
        rec.resources.iter().any(|r| r.kind == EscapeKind::Ffi),
        "{:?}",
        rec.resources
    );
    assert!(
        msg.contains("(FFI handle)"),
        "FFI in hint: {msg} {:?}",
        rec.resources
    );
    assert!(
        rec.resources.iter().any(|r| r.name == "lib")
            || rec.resources.iter().any(|r| r.name == "invoke")
            || rec.resources.iter().any(|r| r.name == "dload"),
        "{:?}",
        rec.resources
    );
}

#[test]
fn heap_index_store_is_named() {
    let ast = parse(
        r#"
fn rec(Vec<int> a, int n) -> int {
    if n <= 1 {
        a[0] = n;
        return n;
    }
    return rec(a, n - 1) + rec(a, n - 2);
}
fn main() { return; }
"#,
    );
    let rec = hints_of(&ast)
        .into_iter()
        .find(|h| h.fn_name == "rec")
        .expect("rec hint");
    assert!(
        rec.resources
            .iter()
            .any(|r| r.name == "a" && r.kind == EscapeKind::HeapObject),
        "{:?}",
        rec.resources
    );
    assert!(rec.message().contains("`a` (user object)"));
}

#[test]
fn stdin_is_a_named_fd_edge() {
    let ast = parse(
        r#"
fn leaf(int n) -> int {
    let s = stdin();
    read(s, n);
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return leaf(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() { return; }
"#,
    );
    let rec = hints_of(&ast)
        .into_iter()
        .find(|h| h.fn_name == "rec")
        .expect("rec hint");
    assert!(
        rec.resources
            .iter()
            .any(|r| r.name == "s" || r.name == "stdin")
            && rec.resources.iter().any(|r| r.kind == EscapeKind::Fd),
        "{:?}",
        rec.resources
    );
}

#[test]
fn host_io_ffi_mutex_names_are_classified() {
    for name in [
        "stdin",
        "stdout",
        "stderr",
        "open",
        "read",
        "write",
        "write_all",
        "write_from",
        "close",
        "connect",
        "listen",
        "accept",
        "bind",
        "send_to",
        "recv_from",
        "tcp_connect",
        "udp_bind",
        "fs_exists",
        "dload",
        "declare",
        "invoke",
        "with_lock",
        "lock",
        "unlock",
        "with_read",
        "with_write",
    ] {
        assert!(
            is_fd_endpoint(name) || is_ffi_host(name) || is_mutex_host(name),
            "unclassified lockable host `{name}`"
        );
    }
    assert!(!is_fd_endpoint("format"));
    assert!(!is_fd_endpoint("to_bytes"));
    assert!(!is_fd_endpoint("panic"));
}

#[test]
fn ffi_and_fd_multi_edge_lists_both() {
    let ast = parse(
        r#"
fn leaf(int n) -> int {
    write(stdout(), n);
    invoke(lib, 0, n);
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return leaf(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() { return; }
"#,
    );
    let rec = hints_of(&ast)
        .into_iter()
        .find(|h| h.fn_name == "rec")
        .expect("rec hint");
    let msg = rec.message();
    assert!(
        msg.contains("`stdout` (FD)") && msg.contains("(FFI handle)"),
        "both edges: {msg}"
    );
}

#[test]
fn pure_fib_has_no_escape_hint() {
    let ast = parse(
        r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() { return; }
"#,
    );
    let hints = hints_of(&ast);
    assert!(hints.is_empty(), "pure IPA must not hint: {hints:?}");
}

#[test]
fn panic_impurity_is_not_a_lock_hint() {
    let ast = parse(
        r#"
fn rec(int n) -> int {
    if n <= 1 { panic("x"); }
    return rec(n - 1) + rec(n - 2);
}
fn main() { return; }
"#,
    );
    let hints = hints_of(&ast);
    assert!(hints.is_empty(), "panic is not lockable: {hints:?}");
}

/// `fn_name: edges` for each hint, with ` [lock] at <callee>` for a
/// covering lock: the source text its span covers.
fn summary(src: &'static str) -> Vec<String> {
    hints_of(&parse(src))
        .iter()
        .map(|h| {
            let lock = if h.covering_lock { format!(" [lock] at `{}`", &src[h.span.clone()]) } else { String::new() };
            format!("{}: {}{lock}", h.fn_name, h.edges_text())
        })
        .collect()
}

/// Qualified host calls, a method call, a construction-shaped call.
#[test]
fn a_host_is_named_through_a_module_path() {
    let hints = summary(
        r#"
use io;
use string::{format, to_bytes};
class Sink { pub n: int, }
impl Sink {
    pub fn open() -> int { return 1; }
}
fn leaf(int n) -> int {
    io::write(io::stderr(), to_bytes(format("%i", n)));
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return leaf(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() { return; }
"#,
    );
    assert_eq!(
        hints,
        [
            "rec: `stderr` (FD)",
        ]
    );
}

/// Heap stores, element reads on the left of an operator, append.
#[test]
fn stores_and_operator_lefts_name_their_object() {
    let hints = summary(
        r#"
class Box { pub v: int, }
fn bump(Box b, Vec<int> xs, int n) -> int {
    b.v = b.v + 1;
    xs[0] += n;
    xs.push(n);
    let k = xs[1] * 2;
    return k;
}
fn rec(Box b, Vec<int> xs, int n) -> int {
    if n <= 1 { return bump(b, xs, n); }
    return rec(b, xs, n - 1) + rec(b, xs, n - 2);
}
fn main() { return; }
"#,
    );
    assert_eq!(
        hints,
        [
            "rec: `b` (user object), `xs` (user object)",
        ]
    );
}

/// Transitive callees, a lambda.
#[test]
fn escapes_are_followed_through_callees_and_lambdas() {
    let hints = summary(
        r#"
use io::{stdout, stdin, write, read};
use string::{to_bytes};
fn deep(int n) -> int {
    write(stdout(), to_bytes("x"));
    return n;
}
fn inner(int k) -> int {
    let buf = read(stdin(), 1);
    return k;
}
fn mid(int n) -> int {
    let f = fn (int q) => deep(q);
    return inner(n) + f(n);
}
fn rec(int n) -> int {
    if n <= 1 { return mid(n); }
    return rec(n - 1) * rec(n - 2);
}
fn main() { return; }
"#,
    );
    assert_eq!(
        hints,
        [
            "rec: `stdin` (FD), `stdout` (FD)",
        ]
    );
}

/// Locks by bare name and by path, the bag under `if` and `match`.
#[test]
fn a_lock_is_found_by_bare_name_and_by_path() {
    let hints = summary(
        r#"
use thread;
use thread::{mutex, with_lock, with_read, lock, unlock};
fn a(Mutex m, int n) -> int {
    if n <= 1 { return 1; }
    return thread::with_lock(m, fn (int x) => a(m, n - 1) - a(m, n - 2))?;
}
fn b(Mutex m, int n) -> int {
    if n > 1 {
        let r = with_read(m, fn (int x) => b(m, n - 1) ^ b(m, n - 2))?;
        return r;
    }
    return 1;
}
fn c(Mutex m, int n) -> int {
    lock(m);
    let r = c(m, n - 1) + c(m, n - 2);
    unlock(m);
    return r;
}
fn main() { return; }
"#,
    );
    assert_eq!(
        hints,
        [
            "a: `m` (mutex) [lock] at `thread::with_lock(m, fn (int x) => a(m, n - 1) - a(m, n - 2))`",
            "b: `m` (mutex) [lock] at `with_read`",
            "c: `m` (mutex)",
        ]
    );
}

/// Ffi, a stream bound to a local, `?` around the stream.
#[test]
fn ffi_handles_and_bound_streams_are_named() {
    let hints = summary(
        r#"
use io::{open, write, close};
use string::{to_bytes};
fn dump(int n) -> Result<int, string> {
    let f = open("out.txt", "w")?;
    write(f, to_bytes("x"))?;
    close(f)?;
    return Ok(n);
}
fn poke(int n) -> int {
    let lib = dload("libc.so.6");
    let h = declare(lib, "abs", 1, 1);
    return invoke(h, n);
}
fn rec(int n) -> int {
    if n <= 1 { return poke(n); }
    return rec(n - 1) + rec(n - 2);
}
fn rec2(int n) -> int {
    if n <= 1 { return 1; }
    let a = dump(n) ?? 0;
    return rec2(n - 1) + rec2(n - 2);
}
fn main() { return; }
"#,
    );
    assert_eq!(
        hints,
        [
            "rec: `declare` (FFI handle), `dload` (FFI handle), `h` (FFI handle), `invoke` (FFI handle), `lib` (FFI handle)",
            "rec2: `f` (FD), `open` (FD)",
        ]
    );
}

/// Nothing under `while let` / `new`; `if let` and loops entered.
#[test]
fn new_is_not_entered_and_if_let_and_loops_are() {
    let hints = summary(
        r#"
use io::{stdout, stderr, write};
use string::{to_bytes};
class P { pub v: int, }
fn quiet(Option<int> o, int n) -> int {
    if let Some(v) = o { write(stdout(), to_bytes("a")); }
    let p = new P(n);
    return p.v;
}
fn loud(int n) -> int {
    let i = 0;
    while i < n { write(stderr(), to_bytes("b")); i = i + 1; }
    for k in 0..n { write(stdout(), to_bytes("c")); }
    return n;
}
fn rec(Option<int> o, int n) -> int {
    if n <= 1 { return quiet(o, n); }
    return rec(o, n - 1) + rec(o, n - 2);
}
fn rec2(int n) -> int {
    if n <= 1 { return loud(n); }
    return rec2(n - 1) + rec2(n - 2);
}
fn main() { return; }
"#,
    );
    assert_eq!(
        hints,
        [
            "rec: `stdout` (FD)",
            "rec2: `stderr` (FD), `stdout` (FD)",
        ]
    );
}

/// A construction-shaped call is no call, its arguments are read; a method
/// call names no host; a local called by a host's name does (in a function
/// that is impure for another reason: a pure one gets no hint).
#[test]
fn a_call_is_read_as_it_is_written() {
    let hints = summary(
        r#"
use io::{stdout, stderr, write};
use string::{to_bytes};
static let SEEN: int = 0;
class Sink { pub n: int, }
impl Sink {
    pub static fn open(int n) -> int { return n; }
    pub fn close() -> int { return self.n; }
}
fn through(Sink s, int n) -> int {
    let a = Sink::open(n);
    let b = s.close();
    return a + b;
}
fn args(int n) -> int {
    let a = Sink::open(count(n));
    return a;
}
fn count(int n) -> int {
    write(stderr(), to_bytes("n"));
    return n;
}
fn shadow(int n) -> int {
    let write = fn (int q) => q;
    SEEN = SEEN + 1;
    return write(n);
}
fn rec(Sink s, int n) -> int {
    if n <= 1 { return through(s, n); }
    return rec(s, n - 1) + rec(s, n - 2);
}
fn rec2(int n) -> int {
    if n <= 1 { return args(n); }
    return rec2(n - 1) + rec2(n - 2);
}
fn rec3(int n) -> int {
    if n <= 1 { return shadow(n); }
    return rec3(n - 1) + rec3(n - 2);
}
fn main() { return; }
"#,
    );
    assert_eq!(
        hints,
        [
            "rec2: `stderr` (FD)",
            "rec3: `n` (FD)",
        ]
    );
}

/// Tuples, a static, compound stores in a loop, `&&` and shifts.
#[test]
fn fields_elements_and_tuples_name_their_base() {
    let hints = summary(
        r#"
static let LOG: Vec<int> = Vec::new();
class Acc { pub total: int, pub items: Vec<int>, }
fn fold(Acc acc, (int, int) pair, Vec<int> xs, int n) -> int {
    let i = 0;
    while i < n && xs[i] > 0 {
        acc.total += xs[i] << 1;
        acc.items[i] = pair[0];
        i = i + 1;
    }
    let t = (acc.total, xs[0] >> 1);
    return t[0];
}
fn rec(Acc acc, (int, int) pair, Vec<int> xs, int n) -> int {
    if n <= 1 { return fold(acc, pair, xs, n); }
    return rec(acc, pair, xs, n - 1) - rec(acc, pair, xs, n - 2);
}
fn main() { return; }
"#,
    );
    assert_eq!(
        hints,
        [
            "rec: `acc` (user object), `xs` (user object)",
        ]
    );
}

/// A lock around a bag in a tuple, in a call argument and behind a named
/// argument; a lock with no bag under it.
#[test]
fn a_lock_is_found_in_a_tuple_and_behind_a_named_argument() {
    let hints = summary(
        r#"
use thread::{mutex, with_lock, with_write};
fn keep(int a, int b) -> int { return a + b; }
fn a(Mutex m, int n) -> int {
    if n <= 1 { return 1; }
    let pair = (0, with_lock(m, fn (int x) => keep(a(m, n - 1) * a(m, n - 2), 0))?);
    return pair[1];
}
fn b(Mutex m, int n) -> int {
    if n <= 1 { return 1; }
    return keep(b: with_write(m, fn (int x) => b(m, n - 1) + b(m, n - 2) + b(m, n - 3))?, a: 0);
}
fn c(Mutex m, int n) -> int {
    if n <= 1 { return 1; }
    return with_lock(m, fn (int x) => c(m, n - 1))? + n;
}
fn main() { return; }
"#,
    );
    assert_eq!(
        hints,
        [
            "a: `m` (mutex) [lock] at `with_lock`",
            "b: `m` (mutex) [lock] at `with_write`",
        ]
    );
}
