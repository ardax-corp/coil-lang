# Macros: user derives and attribute macros

Packages define `derive`s and attribute macros in coil. The compiler runs them
in the VM at compile time, after parsing and before typechecking, and splices
their output next to (derive) or in place of (attribute macro) the declaration.
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

- `derive Name(TypeDecl t) -> Code attrs(h, …) { … }` declares a derive. `attrs(...)`
  lists the field / variant helper attributes it owns.
- `attr name(FnDecl f, …) -> Code` or `attr name(TypeDecl t, …) -> Code` declares an
  attribute macro. The extra parameters bind the attribute's arguments, by name
  (`key = value`) or in order, as literals. The runtime `target(...args)`
  decorators these replaced are gone; that form is now an error.
- Several attribute macros on one item apply outermost first: only the first
  runs, and it receives the item with the others still on it (and a type's
  derives, which run after it) — `FnDecl.with_name` keeps them.
- `derive`, `attrs` and `quote` are contextual words, not reserved identifiers.
- Macros are imported with `use` like any item. Derives have their own namespace,
  so `json::ToJson` can be both a trait and its derive. The same name imported
  from two modules is an error.

## The `macro` module

`compiler/src/prelude/macro.hy` is coil source embedded in the compiler
(`include_str!`) and served as module `macro` at the pseudo path
`<coil>/macro.hy`. It describes declarations **as written**. Types are not
resolved: a derive emits `self.x.show()` and the typechecker reports a missing
`Show`, as Rust does.

| Type | Members |
|------|---------|
| `TypeDecl` | `name: Ident`, `kind` (`"class"` / `"enum"`), `generics`, `fields()`, `variants()`, `attrs`, `repr` (scalar backing or `""`), `module`, `docs`, `source`, `is_class()`, `is_enum()`, `has_attr`, `attr_str` |
| `Field` | `name: Ident`, `ty: TypeRef`, `is_pub`, `attrs`, `docs`, `has_attr`, `attr_str` |
| `Variant` | `name`, `shape` (`"unit"` / `"tuple"` / `"record"`), `tuple: Vec<TypeRef>`, `fields: Vec<Field>`, `value` (discriminant as written), `arity()` |
| `TypeRef` | `str()`, `head()`, `args()` |
| `Attr` / `AttrArg` | `name`, `args`, `has(key)`, `arg(key, fallback)` |
| `FnDecl` | `name`, `params: Vec<Param>`, `ret`, `type_params`, `attrs`, `owner` (class of an `impl` method), `is_static`, `is_coro`, `body_source()` (braces included), `signature(name)`, `arg_names()`, `source` |
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

- `${e}` splices `e.src()`: an `Ident`, a `TypeRef` or a `Code`. Wrap a string
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
depends on the user's imports. No hidden `use` lines are added to user code.
The provider module itself gets `use macro::{Code, join}` added when its quotes
need them.

## Pipeline stage

```
discover_all → expand_user_macros → (discover newly used modules) → typecheck / codegen
```

1. **Attribute expansion** (`attrs::expand_program_in`, per file, during
   discovery) lowers macro items and records every derive, and each item's
   first attribute that is not built in (`derive`, `repr`, `max_depth`, …),
   as a `PendingMacro`. It still adds the type-name `Show` / `String`
   defaults. A field / variant attribute on a type with no derive is an
   error right away.
2. **Resolve** each pending name through the file's top-level `use` items to a
   module whose `CachedAst::macro_decls` declares it; a built-in derive name
   that is not imported resolves to the embedded `derive` module (which only
   the expansion program imports, so it is never compiled into the user's
   program). Nothing found: "Cannot derive unknown or non-derivable trait" /
   "Unknown attribute". A module using its own macro is a staging error.
3. **Encode** each call's input as coil source (`macros::encode`). Every object
   is bound to its own `let`, so no constructor is nested in another's
   arguments.
4. **Compile** one expansion program (`<coil>/expand.hy`: `use` of each
   provider function under an alias, one `fn __coil_expand_i() -> string` per
   call) in a sub-`Pipeline` with the same roots. The sub-pipeline expands its
   own modules' macros the same way. Files already being expanded by an
   enclosing pipeline are on `macro_stack`, and reaching one again is reported as
   an expansion cycle.
5. **Run** every entry through the `MacroHost` (below). Outputs are cached for
   the life of the process, keyed by the sources of the provider and its
   dependencies plus the macro and its encoded input
   (`Pipeline::job_key`), so the LSP does not recompile providers on every
   keystroke.
6. **Splice.** `CachedAst::parse_generated` parses each output padded with
   spaces, so its spans sit after the end of the file (and of earlier
   snippets). Spans never collide with the file's own or with the synthetic
   `0x4000_0000+` spans the `Show` / `String` defaults use. Macro uses in
   generated code (stacked attributes, derives on a generated type) run in the
   next round, up to 16 rounds. Derive outputs keep the order the derives are
   listed in. A derive
   that writes `impl Show for T` replaces the compiler's default type-name
   `Show` (same for `String`); an attribute macro that replaces a type drops
   the old type's defaults.
7. **Diagnostics.** A message whose range falls inside generated code moves to
   the macro's use site with `in code generated by derive X: <line>` as help
   (`Pipeline::remap_generated`). Sinks render against
   `CachedAst::report_source` (file + generated text).

`coil dissect --expand FILE` prints the entry file after expansion. The LSP
checks projects through `typecheck_project`, which runs the same stage, and
offers "Expand macros in this file" on attribute lines.

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
`* 31 +` from the variant index, `Default` zeroes, and scalar-backed enums
compare / show their backing. When they replaced the Rust code, every
derive-using program compiled to identical bytecode except enums with tuple
variants: their payloads are bound by the pattern (`E::V(s_p0)`) since the old
`p.0` field access cannot be written in source.
`comptime/tests/derive_golden.rs` pins the generated source.
`Serialize` / `Deserialize` were removed (placeholders that cast fields to a
byte); serializers belong in format packages as user derives.

## Not yet

- Derives on generic types are refused, as for built-ins:
  `impl<T: Show> Show for Box<T>` does not parse, and instances carry no
  constraints.
- Generated code names a provider trait by path (`impl derive_macros::Summary
  for Point`, `tests/positive/derive_macro_package_trait.hy`), so it relies on
  qualified `impl` heads, which user code has no reason to write.
- Function-style `name!(…)` macros and `comptime` (stages 3–4 of the macro
  design) are not started.
- The expansion cache is in memory only: a fresh process compiles the
  providers once more.
