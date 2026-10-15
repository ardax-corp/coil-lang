# Language tests

`coil test` walks this directory. Each subdirectory is one corpus, named for
the part of the language it covers:

| Corpus | Covers |
|--------|--------|
| `syntax` | literals, comments, blocks, statements, `let` / `const`, shadowing, precedence |
| `types` | primitive types, arithmetic and bitwise semantics, overflow, casts, inference, aliases, `readonly` |
| `control_flow` | `if`, `while`, `for … in`, `break` / `continue`, `return`, `defer` |
| `functions` | calls, named / rest / spread arguments, overloads, recursion, tail calls, lambdas, function values |
| `data` | classes, records, enums, tuples, fields, methods, constructors |
| `patterns` | `match`, destructuring, exhaustiveness |
| `collections` | fixed arrays, `Vec`, dicts, ranges, indexing, element-wise operators |
| `strings` | string operations, formatting, `Show` output |
| `errors` | `Option`, `Result`, `?`, `??`, `raise`, `panic` |
| `generics` | generic functions, classes and enums, bounds, monomorphization |
| `traits` | traits and impls, operator traits, `derive`, existentials, static trait methods |
| `effects` | `pure` / `uses` effect declarations |
| `tasks` | coroutines, tasks, threads, channels, cancellation |
| `contracts` | `requires` / `ensures` |
| `macros` | derive, attribute and function macros |
| `modules` | `use`, visibility, qualified paths, statics and consts |
| `memory` | GC roots, finalizers, weak handles, value layout |
| `host` | IO, files, env, clock, sockets, FFI, capability grants |
| `codegen` | programs that pin a specific optimization or lowering (inlining, CSE, unrolling, SROA, auto-par, SIMD); still asserted on behaviour |
| `testing` | the harness itself: `test` blocks, `assert!`, `?` in cases |

Inside a corpus:

- a `.hy` file with `test("…") { … }` cases runs each case; a file with only
  `fn main()` runs it and compares stdout with its `// Output:` header when it
  has one;
- `compile_fail/` holds programs the compiler must reject with one of the
  error codes on the file's `// Expected: E0102 — why` header lines.

Run one corpus with `coil test tests/<corpus>`, or every corpus in parallel
with `scripts/test-corpora.sh` (see [test-runner](../docs/internals/test-runner.md)).
