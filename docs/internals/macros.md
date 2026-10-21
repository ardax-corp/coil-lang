# Macros: derives, attribute macros and function-style macros

Packages define `derive`s, attribute macros and function-style macros in coil.
The compiler runs them in the VM at compile time, after parsing and before
typechecking, and splices their output next to (derive) or in place of
(attribute macro) the declaration, or in place of the `name!(…)` call.
The built-in derives (`Show`, `Eq`, `Ord`, `Hash`, `Default`, `Send`,
`Sensitive`) are derive macros too, in `compiler/src/prelude/derive.hy`.

Code: `compiler/src/macros/` (lowering, input encoding, `MacroHost`),
`compiler/src/pipeline_macros.rs` (the pipeline stage), `compiler/src/prelude/macro.hy`
(the embedded `macro` module), `comptime/` (the VM host).

## Surface

```coil
// provider: examples/src/derive_macros.hy
use macro::{TypeDecl, FnDecl, Code, lit, raw, ident};

/// `T::field_names()`; `#[names(rename = "…")]` on a field renames it.
derive FieldNames(TypeDecl t) -> Code attrs(names) {
    let names: Vec<Code> = Vec::new();
    for f in t.fields() {
        names.push(lit(f.attr_str("names", "rename", f.name.str())));
    }
    return quote items {
        impl ${t.name} {
            pub static fn field_names() -> Vec<string> {
                let out: Vec<string> = Vec::from([$(names),*]);
                return out;
            }
        }
    };
}

/// An attribute macro: gets the item, returns what replaces it.
attr add_after(FnDecl f, int by) -> Code { … }
```

```coil
// user
use derive_macros::{FieldNames, add_after};

#[derive(FieldNames)]
class Config {
    #[names(rename = "server-port")]
    pub port: int,
}

#[add_after(by = 10)]
fn triple(int x) -> int { return x * 3; }
```

```coil
// provider: examples/src/fn_macros.hy
use macro::{Expr, Code, lit};

/// A function-style macro: gets each argument as an `Expr`.
macro check(Expr cond) -> Code {
    return quote stmts {
        if !${cond} {
            panic "check failed: " + ${lit(cond.str())};
        }
    };
}
```

```coil
// user
use fn_macros::{square, check, counter};

counter!(Clicks);          // item: the output is declarations

fn main() {
    let x = square!(1 + 2); // expression: the output is one expression
    check!(x == 9);         // statement: the output is statements
}
```

- `derive Name(TypeDecl t) -> Code attrs(h, …) { … }` declares a derive. `attrs(...)`
  lists the field / variant helper attributes it owns.
- `attr name(FnDecl f, …) -> Code` or `attr name(TypeDecl t, …) -> Code` declares an
  attribute macro. The extra parameters bind the attribute's arguments, by name
  (`key = value`) or in order, as literals. The runtime `target(...args)`
  decorators these replaced are gone; that form is now an error.
- Several attribute macros on one item apply outermost first: only the first
  runs, and it receives the item with the others still on it (and a type's
  derives, which run after it) — `FnDecl.with_name` keeps them.
- `macro name(Expr a, …, Vec<Expr> rest) -> Code` declares a function-style
  macro, used as `name!(…)` (see [Function-style macros](#function-style-macros)).
- `derive`, `attrs`, `macro` and `quote` are contextual words, not reserved
  identifiers (`use macro::{…}` names the module).
- Macros are imported with `use` like any item. Derives and function-style
  macros have their own namespaces, so `json::ToJson` can be both a trait and
  its derive. The same name imported from two modules is an error.

## The `macro` module

`compiler/src/prelude/macro.hy` is coil source embedded in the compiler
(`include_str!`) and served as module `macro` at the pseudo path
`<coil>/macro.hy`. It describes declarations **as written**. Types are not
resolved: a derive emits `self.x.show()` and the typechecker reports a missing
`Show`, as Rust does.

| Type | Members |
|------|---------|
| `TypeDecl` | `name: Ident`, `kind` (`"class"` / `"enum"`), `generics`, `fields()`, `variants()`, `attrs`, `repr` (scalar backing or `""`), `module`, `docs`, `source`, `is_class()`, `is_enum()`, `self_type()` (`Name` / `Name<T, …>`), `impl_head(bound)` (`Name` / `Name<T: bound, …>`), `has_attr`, `attr_str` |
| `Field` | `name: Ident`, `ty: TypeRef`, `is_pub`, `attrs`, `docs`, `has_attr`, `attr_str` |
| `Variant` | `name`, `shape` (`"unit"` / `"tuple"` / `"record"`), `tuple: Vec<TypeRef>`, `fields: Vec<Field>`, `value` (discriminant as written), `arity()` |
| `TypeRef` | `str()`, `head()`, `args()` |
| `Attr` / `AttrArg` | `name`, `args`, `has(key)`, `arg(key, fallback)` |
| `FnDecl` | `name`, `params: Vec<Param>`, `ret`, `type_params`, `attrs`, `owner` (class of an `impl` method), `is_static`, `is_coro`, `declares_effects`, `is_pure` (`pure fn`), `effects` (the `uses {…}` names), `uses_clause()`, `requires` / `ensures` (each clause as written after its keyword), `contract_clauses()`, `body_source()` (braces included), `signature(name)`, `arg_names()`, `source` |
| `Expr` | a `name!(…)` argument as written: `str()` (source text), `src()` (parenthesized unless a single term), `kind()` (`"literal"` / `"ident"` / `"path"` / `"call"` / `"other"`), `is_literal()`, `is_ident()`, and the constant value when the argument folds to one: `int()`, `float()` (an int is accepted too), `bool()`, `string()` (escapes decoded), each an `Option` that is `None` otherwise |
| `Code` | generated source: `src()` |
| helpers | `lit(string)` (string literal), `lit_int`, `raw(text)`, `ident(name)`, `join(Vec<Code>, sep)`, `concat` |

`Expr` values come from `macros::encode::expr_value`: a string literal is
decoded as written; anything else goes through `const_fold::eval_expr` with no
bindings, so `2 * 2` is `int 4` but a variable or a call is not a constant.
Folded string expressions (`"a" + "b"`) are not offered yet because the folder
keeps some escapes undecoded (#1810). A float is sent as decimal digits and a power of
ten and rebuilt exactly when the digits fit in 53 bits and the exponent in 22.

Everything in the model is a class with inherent methods; there are no enum
payloads, traits or type aliases: cross-module aliases and enum payloads of
module classes did not resolve when this was written. A `Splice` trait
(`impl Splice for string`) could later let `${"text"}` splice a literal
directly.

## Quote

`quote items|expr|stmts|type { template }` is an expression of type `Code`.
The parser keeps the template as text with holes (`Expression::Quote`), and
`macros::lower` turns it into string building:

- `${e}` splices `e.src()`: an `Ident`, a `TypeRef`, an `Expr` or a `Code`. Wrap a string
  with `lit(...)` (literal) or `raw(...)` (source).
- `$(xs) sep *` splices a `Vec<Code>` with `sep` (up to two characters, possibly
  none) between the elements.
- Holes inside string literals are plain text. Braces in the template must
  balance.

The template is parsed for real only when the output is: a template error
shows up as "produced code that does not parse", with the generated code
attached. Parsing it with placeholder holes at declaration time gave false
positives (holes that stand for match arms or statement lists), so it was
dropped.

**Hygiene.** Names bound by `let` / `for` in any quote of a macro are renamed
`name__m` in all of that macro's quotes, so they can't capture names in code
spliced from the user. Bare names of the provider module's own items
(`Tag`, `tag_prefix()`) are written as `module::Tag`, so an expansion never
depends on the user's imports. That includes a provider's trait in an `impl`
head (`impl derive_macros::Summary for Point`,
`tests/macros/derive_macro_package_trait.hy`); only macro output may write a
qualified `impl` head, hand-written source gets an error suggesting `use`
(`attrs::expand_source_in`). No hidden `use` lines are added to user code.
The provider module itself gets `use macro::{Code, join}` added when its quotes
need them.

## Pipeline stage

```
discover_all → expand_user_macros → (discover newly used modules) → typecheck / codegen
```

1. **Attribute expansion** (`attrs::expand_program_in`, per file, during
   discovery) lowers macro items and records every derive, each item's
   first attribute that is not built in (`derive`, `repr`, `max_depth`, …),
   and every outermost `name!(…)` call, as a `PendingMacro`. It still adds the type-name `Show`
   default. A field / variant attribute on a type with no derive is an
   error right away.
2. **Resolve** each pending name through the file's top-level `use` items to a
   module whose `CachedAst::macro_decls` declares it; a built-in derive name
   that is not imported resolves to the embedded `derive` module (which only
   the expansion program imports, so it is never compiled into the user's
   program). A use in macro output (`PendingMacro::from_provider`) that the
   file it lands in does not import resolves to its provider's own macros,
   then to the macros the provider imports with its own top-level `use`, so
   `quote expr { square!(…) }` needs no import where it lands (#1407).
   Nothing found: "Cannot derive unknown or non-derivable trait" /
   "Unknown attribute" / "unknown macro `name!`" (for a call in macro output
   the help names the provider to declare or import it in). A module using
   its own macro is a staging error.
   A name that does not resolve waits a round instead when a module it would
   be imported from still has pending macros this round, or when its own file
   has other item-level ones: their output may declare it (below).
3. **Encode** each call's input as one string (`macros::encode::Wire`):
   length-prefixed fields (`<len>:<bytes>`, lists as a count then their
   items) in the order the model's constructors take them, then the
   attribute arguments in parameter order; a call's arguments are
   `(kind, source text)` each, the rest as a count then its items.
   `macro::Reader` (`r.type_decl()`, `r.fn_decl()`, `r.expr()`, `r.exprs()`,
   `r.str()`, `r.int()`, `r.bool()`) decodes it in the VM. Attribute-macro
   parameters are `string`, `int` or `bool`.
4. **Compile** the expansion program for the round's provider modules
   (`<coil>/expand.hy`: `use` of every macro function each provider declares,
   under an alias, and one `fn __coil_run_i(string input) -> string` wrapper
   per macro that decodes the input and calls it). It depends only on the
   providers, so it is compiled once per provider set and process
   (`COMPILED`, keyed by the provider modules and the sources of the
   providers and their dependencies; parallel `coil test` workers wait for
   one compile, and failures are not cached), and across processes on disk
   (see the limitations below). The compile runs in a sub-`Pipeline` with
   the same roots, grants and result cache, which expands its own modules'
   macros the same way. Files already being expanded by an enclosing pipeline are on
   `macro_stack`, and reaching one again is reported as an expansion cycle.
5. **Run** every call through the `MacroHost` (below) with its input.
   Outputs are cached for the life of the process too, keyed by the provider
   sources, the macro and its input (`Pipeline::job_key`), so the LSP does not
   rerun macros on every keystroke. An output is reused only while the files
   it read are unchanged; one that wrote a file is not reused. A macro that
   did IO also goes to the result cache (see "Compile-time result cache").
6. **Splice.** `CachedAst::parse_generated` parses each output with
   `Pratt::parse_at`, so its spans sit after the end of the file (and of
   earlier snippets), as if it followed them in the text. Spans never collide with the file's own or with the synthetic
   `0x4000_0000+` spans the `Show` defaults use. Macro uses in
   generated code (stacked attributes, derives on a generated type) run in the
   next round, up to 16 rounds. Derive outputs keep the order the derives are
   listed in. Macro items in output at the top level (`macro`, `derive`,
   `attr`, written by a `comptime` block or a macro) are lowered and
   registered in the file's `macro_decls`, so other modules use them like
   hand-written ones from the next round on; their own module may not
   (the staging error). A call's output is parsed for its position (below). A derive
   that writes `impl Show for T` replaces the compiler's default type-name
   `Show`; an attribute macro that replaces a type drops
   the old type's defaults. A call's output replaces the call once every
   output of the round is parsed, in one walk of each file
   (`replace_calls`); calls are looked up by span from one index per file
   and round (`index_calls`).
7. **Diagnostics.** A message whose range falls inside generated code moves to
   the macro's use site with `in code generated by derive X: <line>` as help
   (`Pipeline::remap_generated`). Text the output copies unchanged from the
   input (a declaration as written, a function's body: `f.with_name(…)`)
   is recorded per output (`GeneratedRange::verbatim`); a message inside
   it maps back to where that code was written, through stacked macros
   (#1822). Debug locs map the same way. Sinks render against
   `CachedAst::report_source` (file + generated text).

Expansion is about linear in the number of calls in a file (#1398). Each
output used to be parsed behind padding as long as the file plus every
earlier output, and each call was found (twice) and replaced by walking the
whole tree. `main` with N `assert!(x == 1);` (`use tests::assert;`), release
build, `coil dissect --expand` (expansion only; median of 5 runs, callgrind
instructions):

| N    | before          | after           |
|------|-----------------|-----------------|
| 100  | 0.18 s, 1.16 G  | 0.16 s, 1.14 G  |
| 400  | 0.37 s, 2.54 G  | 0.30 s, 2.21 G  |
| 1600 | 1.82 s, 13.1 G  | 1.07 s, 7.67 G  |

`coil compile` output is byte-identical. What is left per call is constant:
the macro's VM run (about half at N = 1600, including zeroing fresh heap
chunks) and building the parser for each output.

`coil dissect --expand FILE` prints the entry file after expansion. Each run
of top-level items that a macro or an item-level `comptime` block generated
starts with a comment naming it and its position
(`// generated by a `comptime` block at main.hy:3:1`), and the hidden function
and const of a `comptime` expression or value block name the position of
the `comptime` they came from (`Pipeline::expansion_notes`,
`parser::format_program_noted`). The LSP checks projects through
`typecheck_project`, which runs the same stage, and offers "Expand macros in
this file" on attribute lines and `name!(…)` lines.

`--opt-stats` (`coil compile`, `coil dissect`) reports compile-time work:
macro calls and `comptime` blocks run and how many reused a cached result,
consts and `comptime` expressions computed (how many from the result cache),
pure calls tried for folding, and the wall time of each. Compiling a
macro program counts in its time, so a cold build shows the compile and a
warm one only the cache lookups. `--opt-stats-json` has the same numbers as
`comptime_*` fields (times in microseconds).

## Providers in the program

A module that declares macros is compiled into a program like any other
module, minus what only the expansion program needs
(`macros::strip_compile_time_items`): its lowered macro functions, and its
`use macro::…` items once nothing left in the module names what they
import. So the `macro` module's classes (`Code`, `Reader`, …) never meet a
user's classes of the same name, and coverage and `coil mutate` do not count
macro bodies, which only run at compile time. The entry file keeps its
macros, so checking a provider still checks their bodies; expansion
programs (`Pipeline::expansion_program`) keep everything.

## Function-style macros

`macro name(Expr a, Expr b) -> Code { … }` is lowered to
`fn __macro_name(Expr a, Expr b) -> Code`. Every parameter is an `Expr`; a
last `Vec<Expr>` parameter takes the remaining arguments. The arity is checked
at the call ("macro `square!` takes 1 argument(s), found 2").

A module may declare one macro name several times with different arities,
the way functions overload, e.g. an optional trailing argument:

```coil
macro ensure(Expr cond) -> Code { … }            // ensure!(x > 0)
macro ensure(Expr cond, Expr msg) -> Code { … }  // ensure!(x > 0, "x")
```

The call's argument count picks the overload during resolution; with none
matching, the error lists every arity ("macro `ensure!` takes 1 or 2
argument(s), found 3"). As for functions, no two overloads may accept the
same count: equal fixed arities, two rest overloads, or a fixed arity at or
above a rest overload's fixed prefix are `E0121`. Every parameter is an
`Expr`, so arity is the only thing that tells overloads apart. The first
declaration lowers to `__macro_name`, later ones to `__macro_name__1`,
`__macro_name__2`, …. Derives and attribute macros do not overload.

`name!(args)` is an expression atom (`Expression::MacroCall`); `!` must be
directly followed by `(`, so `a != b` and `!x` are unaffected. Each argument
parses as an ordinary expression, so there is no custom grammar; named
arguments and `...` spreads are refused. Only the outermost call of a nest is
expanded in a round: calls in its arguments are part of its input text and
expand in its output.

Where the call is decides what its output is (`CallPosition`):

| Call | Output parses as | Spliced |
|------|------------------|---------|
| `name!(…);` at the top level | items (a program) | in place of the statement |
| `name!(…);` in a block | statements (`fn __coil_m() { <output> }`) | in place of the statement, in the enclosing block |
| anywhere else | one expression (`return (<output>);`) | in place of the call |

Output that does not parse there is "macro `name!` produced code that does not
parse as an expression / statements", with the code as help. Hygiene applies
as for other quotes: a `let tmp` in a `quote stmts` is `tmp__m`, so it does not
clash with the caller's `tmp`. A call that failed to expand stays in the tree;
the checker gives it a fresh type and codegen skips it, so the macro stage's
error is the only one reported.

## Compile-time host

`MacroHost` (`compiler::macros`) keeps the compiler free of `machine`; binaries
call `comptime::install()` at startup and pipelines pick it up. `VmMacroHost`
keeps one `Machine` per compiled expansion program per thread (#1008). The
natives, statics and program are loaded once; each call resets the step
budget and calls the entry, and a call that returns normally ends with
`Machine::reset_isolate_heap` so the next one starts from a clean heap. A call
that panics, errors or runs out of steps drops the machine, and the next call
builds a fresh one. The sandboxed native table is built once per process.

- The host natives are the standard table in ABI order. The fs, `open`,
  stream, `env_var` and `env_exec` natives are gated (`comptime::io_gate`, see
  "Compile-time IO" below); every other native that isn't pure (network,
  clocks, threads, the rest of env) is replaced by a stub that fails the
  evaluation (`native_allowed_at_compile_time`).
- Each call runs within `ComptimeLimits` (`HostGrants::comptime`): a step
  budget (`--comptime-steps`, default 1B loop back-edges + calls, see
  `Machine::set_step_budget`) and a cap on live heap bytes (`--comptime-mem`,
  default 1024 MiB, see `Machine::set_heap_cap`). A runaway macro fails with
  "it ran too long", a hoarding one with "it used too much memory", each
  naming the flag to raise. 1B steps is about 15 s of a tight loop on a
  release build. The cap is checked at GC safepoints after a full collection,
  so garbage never trips it; outgrowing it panics at the next back-edge or
  call. Expansion sub-pipelines inherit the parent's limits.
- A panic fails the macro with its message, so `panic "…"` is the way to
  inspect what a macro received.
- The VM's thread-local output redirect is restored before the machine drops.

## Compile-time IO

A `const`, a `comptime` expression or a macro may read files, write files,
read environment variables and run programs only as the build allows:

- `--comptime-read [MODULE=]PATH`, `--comptime-write [MODULE=]PATH`: files
  under `PATH` (a file or directory).
- `--comptime-env [MODULE=]NAME`, `--comptime-exec [MODULE=]NAME`: one
  variable, one program (matched by file stem).
- `--comptime-net [MODULE=]HOST[:PORT]`: TCP connections to `HOST` (any
  port) or `HOST:PORT` (`tcp_connect` / `tcp_connect_timeout`; the stream
  natives, `await_readable` / `await_writable` and `tcp_peer_addr` … follow
  a granted connect). Listening, accepting and UDP stay unavailable. The
  host is matched as written, so `localhost` does not grant `127.0.0.1`.
- `--comptime-timeout SECS` (default 300): wall-clock time per evaluation
  (`ComptimeLimits::timeout_secs`, clock started by `io_gate::start_clock`).
  Each connect, and each read or write on a connected stream, waits at most
  30 s (`NET_OP_SECS`) and at most until just past the clock; an evaluation
  past its clock fails (E0134 / the macro's error) even when its code
  handled the timeout as an `Err`. Pure code is bounded by the step budget.
- Each flag repeats. `MODULE=` scopes a grant to one module (a module
  path such as `config::load`) and the modules under it (`config::load::…`;
  spool scopes a dependency's grants to its top-level modules); without it the grant covers every module,
  the entry file included. `HostGrants::comptime_io` holds them (`ComptimeGrants`,
  `Scoped<T>`), and `ComptimeGrants::for_module` gives the
  `macros::ComptimeIo` a single evaluation runs with.
- Paths resolve against the compiler's working directory, not the source
  file, and are canonicalized (`macros::absolute_path`), so `..` and symlinks
  cannot leave a granted directory.
- Typecheck (`const_effect_messages`): with no grant for its module, a const
  initializer with any IO effect is E0134 and the help names the flags to
  add. With a grant, effects the grants cover are left to the runtime gate
  (stdlib wrappers carry broad effect sets); reading or writing a
  `static let` is still E0134 (`Summary::statics`).
- Runtime: `io_gate::set_active` installs the grants of the item being
  evaluated (consts per module, macros by their provider module). A refused
  call fails the evaluation with "reading `p` while compiling needs
  `--comptime-read <dir>`"; silent folding and `static let` run with no
  grants, so every gated native fails there.
- Const initializers are not reachable from `main` for the runtime
  `--allow-*` check (E0414): their IO happens while compiling.
- Macro expansion caches key on the provider's grants, so changing them
  re-expands.

## Compile-time result cache

`compiler::comptime_cache` keeps the result of each evaluation that did
granted IO, so the next build does not read the environment or run a program
again:

- What is cached: an explicit const or `comptime` expression (its value, as a
  one-root heap snapshot) and a macro call (its output text) whose
  evaluation made at least one granted IO call and wrote no file. Pure
  evaluations are always computed: they cannot change between builds. The
  VM notes each granted call (`io_gate::start_recording`), with the sha256 of
  what a read saw (`path_digest`: a file's bytes, a directory's sorted names,
  `missing`, plus a symlink's target).
- Key (sha256): cache format, archive version, OS and arch, the const's
  name and word kind (or the provider module, macro and input), its grants,
  and the sources of its module and every module it uses, each with the
  macro output spliced into it (`Pipeline::key_sources`). So an edit to that
  module or a dependency is a miss; an edit elsewhere is not.
- An entry (`<key>.entry` in the cache dir) lists the IO with portable paths
  (relative to the working directory) and the value. A hit needs every
  recorded read to see the same digest. A snapshot's type ids are stored
  with their names (`Checker::type_id_keys`: `class m::P`, record shapes)
  and moved to the current program's ids on load; a type that is gone or
  has a different field count is a miss.
- Location: `--comptime-cache DIR`, else `$COIL_CACHE_DIR/comptime`,
  `$XDG_CACHE_HOME/coil/comptime`, `~/.cache/coil/comptime`. Writes go to a
  temporary file and are renamed into place; a cache that cannot be written
  only costs time.
- `--comptime-refresh`: entries that used the environment, a program or the
  network are ignored and computed again; a value that differs is reported as E0135
  (info). Entries that only read files need no refresh: a changed file is
  already a miss.
- `--comptime-offline`: on a miss, the environment, programs and the
  network are refused (`ComptimeIo::offline`), with an error that says to
  build once without it. File reads still work. Environment values are never stored, only
  names.
- The language server replays: `CacheConfig::replay` (set by `coil lsp`)
  acts as `--comptime-offline`, also refuses compile-time code that writes
  files, and its refusals say to build the project
  (`ComptimeIo::replay`). So the editor shows cached macro and block output
  without touching the network or the environment on each keystroke, and a
  missing or stale entry is a diagnostic that says how to refresh it.
  `--comptime-refresh` is ignored there. A cache made by `coil` in the same
  working directory with the same grants is the one the editor reads.
- A cached const is restored into the evaluating VM instead of calling its
  initializer (`ConstEval::cached`), so later consts read it as usual and
  the program's snapshot is captured as before.

Module statics of providers are not initialised (the expansion program has no
`main`), so macros should not depend on them.

Perf (#1008, release build, 2026-10-16): compiling one file with N
`assert!(x == 1);` calls went from 271 ms to 207 ms at N = 100 (median of 7),
about 0.6 ms saved per call. At N = 400 the build is about 1.25 s either way:
there the time goes to codegen of the one large `main` (`mir::gc::fill_live_roots`
and `stackmap::encode_draft_regs`, about 55% of instructions), and macro
expansion is 17%.

## Built-in derives

`compiler/src/prelude/derive.hy` generates what the former Rust synthesizers
built, as source: `Show` formats fields (`Name { a: %v }`) or
variants (`E::V(%v)`), `Eq` / `Ord` compare field-wise then by variant order
(`Lt` / `Le` / `Gt` / `Ge` plus an empty `Ord`, whose superclass `Eq` takes
its own derive, #1716), `Hash` combines with
`* 31 +` from the variant index, `Default` is a `static fn` that gives each
field its type's default (`0`, `0.0`, `false`, `""`, or `Ty::default()`; an
enum takes its first variant), and scalar-backed enums
compare / show / order their backing (a `#[repr(string)]` enum by its
string's bytes, coil-lang#1722). When they replaced the Rust code, every
derive-using program compiled to identical bytecode except enums with tuple
variants: their payloads are bound by the pattern (`E::V(s_p0)`) since the old
`p.0` field access cannot be written in source.
`comptime/tests/derive_golden.rs` pins the generated source.
`Serialize` / `Deserialize` were removed (placeholders that cast fields to a
byte); serializers belong in format packages as user derives. The `String`
trait (`to_string`, a second copy of `Show`) was removed too: `Show` is the
one display trait, and `#[derive(String)]`, `impl String` and a missing
`to_string` point at it.

**Generic types (#552).** A derive on a generic type expands to a bounded
instance (see [generic-instances.md](generic-instances.md)): every type
parameter is bounded by the trait the body uses, `Show for Box<T: Show>`,
`Ord` (and `Lt` / `Le` / `Gt` / `Ge`) for `Box<T: Ord + Eq>` since the chain
compares with `<` and `==`.
`TypeDecl::impl_head(bound)` writes that head (`Name` or `Name<T: bound, …>`)
and `TypeDecl::self_type()` the type in signatures (`Name` or `Name<T, …>`);
user derives use the same two helpers (`impl ${trait} for
${t.impl_head("Show")}`). `Default` fills a `T` field with `T::default()`;
the primitives have built-in `Default` instances (`0`, `0.0`, `false`, `""`)
so `Box<int>::default()` resolves. Generic types get no type-name `Show` /
`String` default.

## Compile-time consts

A `const` whose initializer does not fold (`const T = table();`) is computed
while compiling, on the same host (`MacroHost::eval_consts`):

- Its initializer must be pure (the HIR effect summary of its `Static` body):
  a read or write of a `static let`, or any IO, is E0134 naming the cause.
- After `finalize_bytecode`, `Pipeline::compute_consts` runs every const's
  `__static_init$NAME` in setup order on one fresh VM, each within
  `ComptimeLimits`, storing each value to its slot so later consts can read
  earlier ones. A panic or a limit is E0134 at the const.
- The values are captured as one `common::HeapSnapshot` (sharing and cycles
  kept; see `machine/src/memory/snapshot.rs`). A const whose value has no
  snapshot form (a function value, a stream or other handle) keeps its
  startup initializer (`ConstEval::startup_ok`); in a `comptime` expression
  it is E0134 naming the path (`H.f`).
- A const whose only effects are calls opening a process handle
  (`stdout`, `stderr`, `stdin`, `dload`, `declare`:
  `STARTUP_HANDLE_NATIVES`) is not computed while compiling at all: it
  keeps its startup initializer. Any other IO still needs a grant.
- The setup region's `CALL init; StoreStatic slot` pairs become `NOOP`s and
  one `RestoreSnapshot` (snapshot appended to the constant pool) at the first
  const's place. The initializer bodies stay in the program.
- Without a host (a pipeline built with no VM linked) consts keep their
  startup initializers.
- A `static let` with a pure initializer is computed the same way, silently
  (`ConstEval::silent`): within the silent tier
  (`ComptimeLimits::SILENT_CALL_STEPS` each, `SILENT_MODULE_STEPS` for the
  module), and one that panics, runs out or has no snapshot form keeps its
  startup initializer. A failed call leaves the VM in an unknown state, so
  the values computed so far move to a fresh VM through a snapshot. The
  restored value is an ordinary heap object, so the static stays mutable.

## Compile-time calls

At `-O2` and above (`OptimizeOptions::call_fold`; `COIL_CALL_FOLD=0` or
`Pipeline::set_call_fold(false)` turns it off) a pure call with constant
arguments is computed while compiling, in the same `eval_consts` run as
the consts (so the call can read them):

- Candidates are functions whose HIR summary is pure (as for consts), not
  generic, a coroutine, a closure or in result mode, returning `int`,
  `float`, `bool`, `byte` or `string` (`Compiler::fold_bodies`, by bind
  name in `fold_fns`).
- After finalize, `Compiler::fold_sites` finds each one-word-result `CALL`
  to a candidate whose arguments are the `CONST` / `STRING` words right
  before it, with no branch landing among them
  (`common::jump_targets`): on every path those words are its arguments.
  A call to an auto-par worker computes the function it runs, without the
  worker's depth argument.
- Each distinct call runs once on the compile-time VM within the silent
  tier (sharing its 20M-step pool with the silent static lets). One that
  panics, runs out or returns a non-string where a string is expected
  stays a call.
- `apply_call_folds` turns the argument words into `NOOP`s and the `CALL`
  into a `CONST` (a negative or wide word from the pool) or `STRING`, then
  builds the precise frame maps again.
- A program with any `fn drop()` folds no call and no silent static let:
  their temporaries would finalize at compile time instead of when the
  program runs.
- The fold happens after the HIR / MIR passes, so a folded value does not
  propagate further (`if is_even(4)` keeps its branch).

`black_box(x)` (prelude, HostInvoke 163 with the `HOST` effect) returns `x`
through a call the optimizer cannot see into; benchmarks use it to keep a
call from being computed while compiling.

## Compile-time expressions

`comptime e` (a prefix at unary-minus precedence, `Expression::Comptime`) is a
computed `const` written inline. `comptime_expr::hoist`, run first by
`const_fold::lift_startup_consts` (at parse, and again after user macros
expand, so macro output may use it too), replaces each one with a read of a
new module const `__comptime_<offset>` placed just before the item that
holds it; a nested `comptime` gets its const first. From there it is a
`const` like any other: E0134 for effects, panics, limits and runtime-only
values, and its value comes from the snapshot. Its synthetic nodes get fresh
spans; `ComptimeConst::span` is the expression's. `comptime_expr::describe`
and `explain_messages` word diagnostics about the hidden const as "this
`comptime` expression", and a name it cannot find gets a help line about
local variables, which the module const cannot see.

`comptime { … }` in an expression (an atom, `Expression::ComptimeBlock`, the
same node as the top-level block) is hoisted too: its body becomes a hidden
function `__comptime_<offset>_body()` with an inferred return type, and the
block becomes `comptime __comptime_<offset>_body()`, so it is a computed
const like any other and may call its own module's functions. Its own
`comptime` expressions get their consts first. The local-variable help
covers the hidden function's body. A unit-valued block (only side effects)
does not lower yet (#1798), and neither does a block-bodied lambda called in
place (#1799), which is why the block is a named function.

## Compile-time blocks

A top-level `comptime { … }` (`Expression::ComptimeBlock`) runs while
compiling and returns `Code`; the items it returns replace it, like the
output of a `name!(…);` call at the top level. It is the code-generating
counterpart of `comptime e`: it runs before typechecking, with the macros, so
it may only use code from other modules, which compile first. Its own
module's functions do not exist yet ("Cannot find function `f`", with help
naming the staging rule).

```coil
use api_gen::classes;
use io::file::read_text;

comptime {
    // built with --comptime-read api
    return classes(read_text("api/schema.json") ?? "{}");
}
```

The tree walkers (`for_each_child`) do not enter the block: its body is
not its module's code. `attrs` records it as a pending macro of kind
`MacroKind::Comptime` at item position, with the diagnostic range on the
keyword. `Pipeline::block_program` builds its expansion program: the file's
top-level `use` items (as written), then

```coil
use macro::Code as __coil_Code;
fn __coil_block() -> __coil_Code { <the block> }
fn __coil_run_0(string input) -> string { return __coil_block().text; }
```

Each block is a program of its own, compiled by the same sub-pipeline as a
provider's and run with its module's grants (`comptime_io_for`). Its keys
(in-process, on-disk program, result cache) hold that text and the sources
of the modules its file uses, not the file itself, so an edit elsewhere in
the file reuses its program and result. When it does not compile, the
sub-pipeline's errors in its body (kept by an `EntrySink`) are reported
where they are in the user's file; a panic's position and backtrace are
rewritten the same way. `return` must give `Code`: `?` does not apply, so
use `??` or `match` on a `Result`. A block inside a function is a value
block instead (see "Compile-time expressions"). A file using its own block's output, or a
cycle through the modules it uses, is the usual expansion-cycle error.

Timing (release build, 2026-10-21): a file with one block calling a
generator module, 244 ms cold, 109 ms with its program from the user cache.

## Not yet

- A function-style macro call names the macro bare (`name!`); a path
  (`m::name!`) does not parse.
- Compiled expansion programs are also kept on disk, in
  `<user cache root>/macro-programs/<key>.prog` (`COIL_CACHE_DIR`, else
  `XDG_CACHE_HOME/coil`, else `~/.cache/coil`; never the `--comptime-cache`
  dir, which may be committed). The key is a sha256 of the host's build id
  (`MacroHost::build_id`, the `coil-build-id` of the comptime crate, so any
  compiler change is a miss), the provider module, the generated
  `expand.hy` text and the provider sources with their dependencies. A file
  holds the entry offsets and the program as an archive, verified again on
  load. Compiling the `assert!` provider costs about 115 ms per process;
  loading it, under 2 ms (release build, 2026-10-21: one-`assert!` file
  146 ms cold, 21 ms warm). Outputs that did IO are kept by the result
  cache.
