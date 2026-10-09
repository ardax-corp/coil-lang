# Contracts

`requires` and `ensures` clauses on functions and methods, checked at run
time (plan steps C0 and C1). Class and loop invariants, `decreases`,
`old(e)` and trait-method contracts are C2; generated tests are C3; a
prover is C4.

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
function_decl ::= … where_clause? effect_clause? contract* (block | ';')
contract      ::= ('requires' | 'ensures') expr (',' string)?
```

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

## Levels

`--contracts=all|requires|off` on `coil`, `coil test` and `coil dissect`
sets `Pipeline::set_contracts`. With no flag, `-O0`, `-O1` and `-Og` check
everything. `-O2` and above (the default) check only `requires`: cheap entry
checks that protect a library from its callers. `coil test` checks
everything unless told otherwise. The level reaches HIR build through
`hir::set_contract_level`, set at the start of every compile.

## Elsewhere

- The macro model's `FnDecl` has `requires` / `ensures` (each clause as
  written after its keyword) and `contract_clauses()`. `with_name` keeps
  them.
- `coil-lsp` hover shows a function's clauses.
