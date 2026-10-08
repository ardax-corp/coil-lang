# Macros: derives, attribute macros and function-style macros

Packages define `derive`s, attribute macros and function-style macros in coil.
The compiler runs them in the VM at compile time, after parsing and before
typechecking, and splices their output next to (derive) or in place of
(attribute macro) the declaration, or in place of the `name!(…)` call.
The built-in derives (`Show`, `Eq`, `Ord`, `Hash`, `String`, `Default`, `Send`,
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
| `FnDecl` | `name`, `params: Vec<Param>`, `ret`, `type_params`, `attrs`, `owner` (class of an `impl` method), `is_static`, `is_coro`, `declares_effects`, `is_pure` (`pure fn`), `effects` (the `uses {…}` names), `uses_clause()`, `body_source()` (braces included), `signature(name)`, `arg_names()`, `source` |
| `Expr` | a `name!(…)` argument as written: `str()` (source text), `src()` (parenthesized unless a single term), `kind()` (`"literal"` / `"ident"` / `"path"` / `"call"` / `"other"`), `is_literal()`, `is_ident()` |
| `Code` | generated source: `src()` |
| helpers | `lit(string)` (string literal), `lit_int`, `raw(text)`, `ident(name)`, `join(Vec<Code>, sep)`, `concat` |

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
`tests/positive/derive_macro_package_trait.hy`); only macro output may write a
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
   and every outermost `name!(…)` call, as a `PendingMacro`. It still adds the type-name `Show` / `String`
   defaults. A field / variant attribute on a type with no derive is an
   error right away.
2. **Resolve** each pending name through the file's top-level `use` items to a
   module whose `CachedAst::macro_decls` declares it; a built-in derive name
   that is not imported resolves to the embedded `derive` module (which only
   the expansion program imports, so it is never compiled into the user's
   program). A use in macro output also resolves to its provider's own
   macros, so `quote expr { square!(…) }` needs no import where it lands.
   Nothing found: "Cannot derive unknown or non-derivable trait" /
   "Unknown attribute" / "unknown macro `name!`". A module using its own
   macro is a staging error.
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
   one compile, and failures are not cached). The compile runs in a
   sub-`Pipeline` with the same roots, which expands its own modules' macros
   the same way. Files already being expanded by an enclosing pipeline are on
   `macro_stack`, and reaching one again is reported as an expansion cycle.
5. **Run** every call through the `MacroHost` (below) with its input.
   Outputs are cached for the life of the process too, keyed by the provider
   sources, the macro and its input (`Pipeline::job_key`), so the LSP does not
   rerun macros on every keystroke.
6. **Splice.** `CachedAst::parse_generated` parses each output padded with
   spaces, so its spans sit after the end of the file (and of earlier
   snippets). Spans never collide with the file's own or with the synthetic
   `0x4000_0000+` spans the `Show` / `String` defaults use. Macro uses in
   generated code (stacked attributes, derives on a generated type) run in the
   next round, up to 16 rounds. Derive outputs keep the order the derives are
   listed in. A call's output is parsed for its position (below). A derive
   that writes `impl Show for T` replaces the compiler's default type-name
   `Show` (same for `String`); an attribute macro that replaces a type drops
   the old type's defaults.
7. **Diagnostics.** A message whose range falls inside generated code moves to
   the macro's use site with `in code generated by derive X: <line>` as help
   (`Pipeline::remap_generated`). Sinks render against
   `CachedAst::report_source` (file + generated text).

`coil dissect --expand FILE` prints the entry file after expansion. The LSP
checks projects through `typecheck_project`, which runs the same stage, and
offers "Expand macros in this file" on attribute lines and `name!(…)` lines.

## Function-style macros

`macro name(Expr a, Expr b) -> Code { … }` is lowered to
`fn __macro_name(Expr a, Expr b) -> Code`. Every parameter is an `Expr`; a
last `Vec<Expr>` parameter takes the remaining arguments. The arity is checked
at the call ("macro `square!` takes 1 argument(s), found 2").

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
runs each entry on a fresh `Machine`:

- The host natives are the standard table in ABI order, with every native that
  isn't pure (IO, fs, network, env, clocks, threads, streams) replaced by a
  stub that fails the macro (`native_allowed_at_compile_time`).
- The step budget is `MACRO_STEP_BUDGET` (20M loop back-edges + calls; see
  `Machine::set_step_budget`). A runaway macro fails with "step budget
  exhausted".
- A panic fails the macro with its message, so `panic "…"` is the way to
  inspect what a macro received.
- The VM's thread-local output redirect is restored before the machine drops.

Module statics of providers are not initialised (the expansion program has no
`main`), so macros should not depend on them.

## Built-in derives

`compiler/src/prelude/derive.hy` generates what the former Rust synthesizers
built, as source: `Show` / `String` format fields (`Name { a: %v }`) or
variants (`E::V(%v)`), `Eq` / `Ord` compare field-wise then by variant order
(`Lt` / `Le` / `Gt` / `Ge` plus an empty `Ord`), `Hash` combines with
`* 31 +` from the variant index, `Default` is a `static fn` that gives each
field its type's default (`0`, `0.0`, `false`, `""`, or `Ty::default()`; an
enum takes its first variant), and scalar-backed enums
compare / show their backing (a `#[repr(string)]` enum orders by declaration,
since strings have no ordering). When they replaced the Rust code, every
derive-using program compiled to identical bytecode except enums with tuple
variants: their payloads are bound by the pattern (`E::V(s_p0)`) since the old
`p.0` field access cannot be written in source.
`comptime/tests/derive_golden.rs` pins the generated source.
`Serialize` / `Deserialize` were removed (placeholders that cast fields to a
byte); serializers belong in format packages as user derives.

**Generic types (#552).** A derive on a generic type expands to a bounded
instance (see [generic-instances.md](generic-instances.md)): every type
parameter is bounded by the trait the body uses, `Show for Box<T: Show>`,
`Ord` (and `Lt` / `Le` / `Gt` / `Ge`) for `Box<T: Ord + Eq>` since the chain
compares with `<` and `==`, `String` by `Show` (it formats with `%v`).
`TypeDecl::impl_head(bound)` writes that head (`Name` or `Name<T: bound, …>`)
and `TypeDecl::self_type()` the type in signatures (`Name` or `Name<T, …>`);
user derives use the same two helpers (`impl ${trait} for
${t.impl_head("Show")}`). `Default` fills a `T` field with `T::default()`;
the primitives have built-in `Default` instances (`0`, `0.0`, `false`, `""`)
so `Box<int>::default()` resolves. Generic types get no type-name `Show` /
`String` default.

## Not yet

- `comptime` (stage 4 of the macro design) is not started.
- A function-style macro call names the macro bare (`name!`); a path
  (`m::name!`) does not parse.
- Compiled expansion programs and outputs are cached in memory only: a fresh
  process compiles each provider set once more (an on-disk cache needs a
  location policy for user projects).
