# Contracts

`requires` and `ensures` clauses on functions and methods, `old(e)` in
`ensures`, `invariant` on classes and loops and `decreases` on `while`
loops, and clauses on trait methods that every impl inherits, checked at
run time (plan steps C0 to C2), tests generated from them (C3), and
`coil verify`, which proves them with an SMT solver (C4).

```coil
fn isqrt(int n) -> int
    requires n >= 0, "negative input"
    ensures result * result <= n
    ensures (result + 1) * (result + 1) > n
{
    // ...
}
```

## Syntax

Clauses come after `where` and `uses {…}`, in any number and order:

```
function_decl ::= … where_clause? effect_clause? fn_clause* (block | ';')
fn_clause     ::= ('requires' | 'ensures') expr (',' string)?
while_loop    ::= 'while' expr (('invariant' | 'decreases') expr (',' string)?)* block
for_loop      ::= 'for' pat 'in' expr ('invariant' expr (',' string)?)* block
class_decl    ::= 'class' name type_params? ('invariant' expr (',' string)?)* '{' fields '}'
```

A loop parses its clauses with the expression parser it already has
(`Pratt::contracts_with`): building a fresh one inside an expression
recurses without end. A clause's span runs from its keyword to the end of
its expression or message.

`requires`, `ensures` and `result` are contextual words, so code that names
a variable `result` keeps working. `coil fmt` puts each clause on its own
line, one level in, with the body brace on its own line
(`parser/src/fmt.rs`, `fmt_function_expr`).

The AST keeps each clause as `parser::ast::Contract`: its kind, expression,
the expression's source text (the default failure message) and the
optional message.

## Typing

`Checker::infer_contracts` (`compiler/src/typechecking/infer/infer_fn.rs`)
types every clause after the body, with the parameters in scope. Each
clause must be a `bool`. In `ensures`, `result` is bound to the declared
return type (the whole `Result<T, E>` for a result-mode function). A
parameter named `result` on a function with `ensures` is an error, and so
is `ensures` on a `gen fn` for now. Clause nodes get their `NodeId`s after
the body's (`typechecking/id.rs`), in the same order the checker visits them.

A clause must have no visible effect: the HIR effect analysis
(`ModuleEffects::contract_violations`) reports E0413 when anything inside a
clause writes, does IO, mutates shared state or suspends. Reading memory is
not an effect, so a `pure fn` can carry contracts. Only the clauses that
are compiled in are checked, so `--contracts=off` skips this check.

## Checks

HIR build (`Cx::function` in `compiler/src/hir/build.rs`) lowers the clauses
when the contract level enables them:

- each `requires` becomes `if !(cond) { panic … }` at the top of the body;
- for `ensures`, every `return v`, including the error return of a `?`,
  becomes `return { let result = v; checks; result }`. A unit body that
  runs off its end gets the checks as its last statement.

The panic message is `contract violated: <keyword> <text> ("<message>") in
<function>`. An `ensures` panic is reported at its clause. A `requires`
panic blames the caller. Its `Builtin::Panic` carries a second argument, and
`emit_hir` turns that into `contract_fail` (HostInvoke **159**,
`HostOp::Task`, archive minor 35). The VM handler adds `, called from
<file:line:col>` from the frame below and prints no location of its own.
Calls of a function with a `requires` are not tiny-inlined
(`Compiler::caller_blame_fns`), and the HIR inliner refuses any body with a
`Builtin`, so the caller always has its own frame.

## `old(e)`

Inside `ensures`, `old(e)` is `e` as it was on entry. The checker types the
call as `e` (`Checker::in_ensures`); HIR build evaluates every `old(e)` into
a local after the `requires` checks (`Cx::old_values`) and lowers the call
to a read of it (`BodyBuilder::olds`, keyed by the call node's address).

## Loops

`Cx::checked_loop` runs the invariants, then the `decreases` checks, at the
top of every iteration, before a `while` loop's condition and before a
`for` loop takes its next item; a `for` loop checks its invariants once
more after the last item. An invariant therefore holds whenever the
condition is tested; a `break` skips the final check. `decreases d` keeps
the previous value in a temp: `d` must be non-negative and lower than last
time, or the panic says `(went negative)` / `(did not decrease)`. A `for`
loop takes no `decreases`: it ends when its items do. Loop clauses are typed
in the scope around the loop, so a `for` binding is not visible in them.

## Class invariants

`class C invariant e { … }` is typed with `self: C` and the class's private
fields visible (`Checker::infer_class_invariants`). HIR build collects the
clauses per class (`class_invariants`) and checks them:

- on every return of a `pub` instance method of an inherent impl, like an
  `ensures` (private helpers may break the invariant for a while; `drop`
  and static methods are not checked);
- after `new C(…)`, as `{ let self = new C(…); checks; self }`, reported as
  `in new C`.

A violation is reported once, however many bodies check the clause.

## Trait methods

A trait method's `requires` and `ensures` hold for every impl of it.
`Pipeline::inherit_trait_contracts` (`pipeline_contracts.rs`) runs after
macro expansion, so derive output is covered too. For each impl method of
a trait method with clauses it:

- re-parses the trait's clauses as generated text
  (`CachedAst::parse_generated`), so each copy has its own spans, node ids
  and effect causes;
- renames the trait's parameters to the impl's, by position (a lambda in a
  clause is left alone, since its own parameters may shadow them);
- puts the copies before the impl's own clauses;
- records a `GeneratedRange` whose site is the impl method's header, so
  diagnostics and panic locations point there ("in code generated by the
  contracts of `Area::area`").

From there the impl method is checked like any function, with the
messages naming the trait's clause text. An impl may add `ensures` but not
`requires` (a caller that knows only the trait could not see it), which is
an error at the impl's clause. The trait is found by path, by `use`, in
the impl's own module, or as the only trait of that name. A trait's
default method body checks its own clauses as a function does.
`compile_src` takes the cached-file path for sources with a trait and a
clause, as it does for `#[`.

## Levels

`--contracts=all|requires|off` on `coil`, `coil test` and `coil dissect`
sets `Pipeline::set_contracts`. `all` adds `ensures`, `old`, invariants and
`decreases` to `requires`. With no flag, `-O0`, `-O1` and `-Og` check
everything. `-O2` and above (the default) check only `requires`: cheap entry
checks that protect a library from its callers. `coil test` checks
everything unless told otherwise. The level reaches HIR build through
`hir::set_contract_level`, set at the start of every compile.

## Generated tests

`coil test` turns contracts into test cases: each function with a
`requires` or `ensures` is called with random arguments, and a failed
`ensures` (or any other panic) fails the case with the arguments that
caused it.

```text
> Test "contract: clamp" failed: clamp(x = 0, lo = -1, hi = -1): contract violated: ensures result >= lo && result <= hi in clamp
```

### `arbitrary`

`compiler/src/prelude/arbitrary.hy` is embedded as module `arbitrary`
(`<coil>/arbitrary.hy`, like `task`):

| Item | Meaning |
|------|---------|
| `Gen` | seeded xorshift source; `size` bounds values (ints in `[-size, size]`, lengths up to `size`), `depth` tracks nesting |
| `Gen::new(seed)`, `g.resize(n)`, `g.below(n)`, `g.int_in(lo, hi)`, `g.one_in(n)`, `g.len()`, `g.enter()` / `g.leave()` / `g.deep()` | building blocks for instances |
| `trait Arbitrary<T> { static fn arbitrary(Gen g) -> T }` | instances for `int`, `byte`, `bool`, `float`, `string`, `Vec<T>` and `Option<T>` |
| `any(g)` | `T::arbitrary(g)`, `T` chosen by the expected type: `let v: Vec<int> = any(g);` |
| `#[derive(Arbitrary)]` | after `use arbitrary::Arbitrary`: a class draws each field, an enum picks a variant and draws its payloads |

Ints avoid huge values, so a case does not end on an int overflow panic
in the code under test. Below `deep()` (depth 4) collections are empty
and derived enums take their first variant without payloads, so recursive
types end. A derived class value may break the class's `invariant`; such a
class needs a hand-written instance. The derive is a macro of the module
itself, so macro resolution follows `use` into embedded modules
(`Pipeline::resolve_macro`).

### Cases

`Pipeline::add_contract_tests` (`pipeline_contract_tests.rs`) runs after
trait contracts are inherited, when `set_contract_runs(n)` is above 0
(`coil test --contract-runs`, default 100), tests are included and the
contract level is not `off`. For the entry file it picks every function
and `pub static` method that:

- has a `requires` or `ensures`, no type parameters, and is not a `gen fn`;
- takes only parameters whose types have an `Arbitrary` instance: the
  primitives, `Vec` / `Option` of such types, and types with an
  `impl Arbitrary` anywhere in the program (derived ones included);
- lives in a file without `fn main` (test cases cannot share a file with
  it).

Instance methods are left out: their receiver must keep the class
invariant. Each target gets `test("contract: f") { … }`, parsed as
generated text with the function's header as its site, plus one aliased
`use arbitrary::{…}` at the top of the file. The case:

1. seeds a `Gen` from the case name (the same arguments every run) and
   grows its size with the calls made, so the first failures are small;
2. draws each argument with `any`, and draws again while a `requires` is
   false (at most ten draws per call over the whole case);
3. calls the function in a child task (`arbitrary::run_case`), so a panic
   comes back as a message instead of ending the job;
4. on a panic, fails with the arguments: primitives and types with a
   `Show` instance print their value (strings quoted, `Vec` / `Option` of
   those via `show_vec` / `show_option`), anything else prints its type.

A function with no parameters is called once. Draws are not shrunk.

### Effects

Random arguments must not delete files or open sockets. Codegen asks the
HIR effect summary of the target (`Compiler::contract_case_allowed`) when it
emits a generated case: anything visible besides `read` and `mutate` (or a
call through a function parameter) and the case is dropped from the
runnable cases in `finalize_bytecode`. Its body is still emitted and listed
until then, so labels and node ids stay in step.

### Project sources

Contracts usually sit in `src/`, which the test root only imports. While
compiling a test file, the pipeline lists the other modules that would get
cases (`Pipeline::contract_sources`). The runner keeps those under the
current directory, outside the test root and `.deps/`, and after the suite
compiles each as the entry (`contracts of N project files`). A file outside
the test root runs only its `contract: ` cases, never its `main` or its own
`test` blocks; none left is `Compiled::Nothing`.

### Mutation testing

`coil mutate`'s baseline is a `coil test` run, so the generated cases cover
project lines like any test and a mutant that breaks an `ensures` is killed
by them (`killed … (src/mathx.hy: contract: twice)`). Clauses themselves
are never mutated: the site walker does not visit them.

## Static verification

`coil verify FILE` (the `coil-verify` helper) proves clauses instead of
testing them. It compiles the file with every check on, captures the entry
module's HIR (`compiler::verify::start_verify_capture`), and turns each
reachable contract panic into an SMT-LIB query (`compiler/src/verify/
encode.rs`) for an external solver: `z3 -in` on PATH by default,
`--solver PATH` or `$COIL_SMT_SOLVER` otherwise. `unsat` proves the check
can never fail.

```text
proved   clamp: ensures result >= lo && result <= hi  [a.hy:5:5]
FAILED   calls_half: call to half: requires x >= 0  [a.hy:15:12]
         counterexample: x = 0
unknown  sum_to: invariant s >= 0  [a.hy:9:17] (possible counterexample n = 1)
```

Exit status 1 when a clause has a counterexample, or with `--strict` when
one is not proved.

### Encoding

Symbolic execution over the HIR body: a state maps locals to terms under a
guard (the condition for execution to reach that point). `if` and `match`
run each branch under its condition and join with `ite`. `return`, `break`
and panics end their path. Every term is bound by a `define-fun`, so joins
share their operands and do not copy them.

- Values:
  - `int` is a 64-bit bit-vector. Int `+ - *` and negation trap on
    overflow, so a path that goes on assumes the exact result fits.
  - `bool` is `Bool`. A `byte` is a bit-vector below 256.
  - `Vec<int>`, `Vec<byte>` and `string` are a length plus an array of
    items. A length is below 2^48.
  - Anything else (records, enums, floats, fields) is a fresh constant.
- Own `requires`: the function's own checks end their failing path without
  a goal, so they are assumptions.
- Calls:
  - A call to a function of the same file is modular. Its `requires` is a
    goal at the call (`call to f: requires …`), and its `ensures` hold of a
    fresh result.
  - `len` and `Vec::push` are modelled.
  - Any other call gets a fresh result and forgets every sequence it could
    reach.
- Panics: indexing outside `0..len`, division by zero and `MIN / -1` panic,
  so a path that goes on excludes them.
- Writes and aliasing: a write through one sequence is seen through its
  aliases, and every other sequence forgets its contents.
- Loops are cut at their head:
  - The leading checks (`invariant`, `decreases`) are goals on entry.
  - Every local the body assigns becomes fresh, the checks are re-run as
    assumptions, and the body runs once from there.
  - Each path back to the head must re-establish them.
  - A `for` loop over `lo..hi` or a sequence binds its variable within
    range. Its exit is any state where the clauses hold.
  - `decreases` is reported as skipped: termination is not checked.
- Exact counterexamples: a query is `exact` when nothing on the way was
  made up. Only an exact model is reported as `FAILED` with a
  counterexample. Any other model is a "possible counterexample" of a
  clause that is not proved: the invariant may just be too weak.

Not yet: dropping proved checks from compiled code, bounds-check facts, and
records, enums and instance methods.

## Elsewhere

- The macro model's `FnDecl` has `requires` / `ensures` (each clause as
  written after its keyword) and `contract_clauses()`. `with_name` keeps
  them.
- `coil-lsp` hover shows a function's clauses.
