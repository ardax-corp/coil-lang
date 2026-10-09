# Contracts

`requires` and `ensures` clauses on functions and methods, `old(e)` in
`ensures`, `invariant` on classes and loops and `decreases` on `while`
loops, and clauses on trait methods that every impl inherits, checked at
run time (plan steps C0 to C2). Generated tests are C3; a prover is C4.

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

## Elsewhere

- The macro model's `FnDecl` has `requires` / `ensures` (each clause as
  written after its keyword) and `contract_clauses()`. `with_name` keeps
  them.
- `coil-lsp` hover shows a function's clauses.
