# Native code (`feature/native`)

Coil can run some functions as machine code made by
[Cranelift](https://cranelift.dev) from their MIR. The VM stays: it runs
comptime and macros, `coil debug`, coverage, wasm, and every body the native
backend does not compile. The plan (AOT `coil package`, heap and task
tiers) is in the project's native-compilation assessment; this page
describes what is implemented.

## Using it

```
coil prog.coil --jit          # compile, JIT what it can, run
COIL_NATIVE_STATS=1 coil prog.coil --jit   # counts on stderr
COIL_NATIVE_STATS=2 coil prog.coil --jit   # plus why each body stays on the VM
```

`--jit` skips the build cache (it needs this compile's MIR).

```
coil compile --jit prog.coil -o prog.ccoil   # archive with a native MIR section
coil run prog.ccoil                          # JITs the section, then runs
coil run --no-jit prog.ccoil                 # VM only
```

`coil run --jit` on an archive without the section warns and runs on the
VM. `coil package` does not use the section yet (step 7 builds AOT
binaries instead).

### Testing against the VM

```
coil test --engine native [PATH]
```

runs a suite with native code: each file's MIR is captured and JIT
compiled, and its cases (and the machines they spawn) run it. A suite must
pass on both engines; CI's `harness native` cell runs every corpus this
way. `--coverage` needs `--engine vm` (it counts what the VM runs).

### The native MIR section

`ArchivedProgram::native_mir` (archive 6.20; 6.0 to 6.19 archives load
with it empty) holds bytes in the `coil-native` section format
(`coil-native/src/section.rs`): the magic `CNMIR`, a format byte, then each
body the backend compiles with its name, bytecode entry PC, entry label,
signature, value types and blocks. Only those bodies and only the MIR the
backend lowers are stored, so the encoder never meets an instruction
outside the tier; the format grows with the tiers and bumps its format byte
when the layout changes. A reader refuses any other format, a truncated
section or trailing bytes, and the run falls back to the VM with a
warning. The section is compiled again on every run: the archive carries
MIR, not machine code.

## Numeric tier (step 2)

A body compiles when:

- every parameter, value and the (one-word) result is `int`, `float` or
  `bool` (`MirTy::I64` / `F64` / `Bool`), or a heap word (below);
- its instructions are `Const`, `Bin` (not int `**`), `Cmp`, `Unary`,
  `Cast`, `Phi`, direct `Call`, and its terminators `Jump`, `Br`,
  one-word `Return`, `Unreachable`;
- every callee compiles too (a fixpoint over the call graph);
- it has no `defer`, no emission refusal and no opaque words.

Such a function is **pure**: it reads nothing but its arguments and does
nothing but return a word. That is what makes the tier simple and exact.

### Heap reads (step 4, first part)

A body may also hold heap words (`HeapRef`, `NicheOpt`, `NicheRes`): take
them as parameters, pass them to calls, return them, move them through
phis, and read through a `HeapRef` with `Index`, `ArrayLen` and
`HeapFieldLoad`. Those three call `machine::native::coil_native_index` /
`_len` / `_field`, which do what the VM instruction does; an index out of
bounds bails. Such a body still never allocates, stores or calls into the
VM, so no collection can run while it holds a heap word and every word
stays a live object. Comparing heap words, stores and allocation come
with the next parts of the tier.

### Bail instead of trap

Native code never raises a Coil panic. Where the VM would trap (int
overflow, division by zero, `MIN / -1`, a shift count outside `0..=63`,
`-MIN`) or where native code would leave its stack budget (256 KiB below the
VM's call), it sets `NativeCtx::bail` and returns. Every native caller checks
the flag after a call and returns too. The VM then runs the original call in
bytecode from its first instruction. Because the function is pure, that
re-run gives the VM's own result, panic message, location and backtrace.

After a bail, calls nested inside the VM's re-run (operand stack base above
the bailed call's) stay in the VM, so a deep recursion bails once, not once
per level (`NativeTable::quiet_above`).

### VM ↔ native

`machine::native` holds the table: entry PC → `extern "C" fn(ctx, *const
u64) -> u64`. `CALL` checks it only when a table is installed
(`unlikely(self.native.is_some())`), and never while a debugger is
attached. The entry wrapper unpacks VM words (a `bool` word is true when
its low byte is `1`, as `Value::as_bool`; a `float` word is its bits), calls
the native body, and packs the result. Native-to-native calls are direct
Cranelift calls with the values in registers.

The table is installed on the program's main machine (`run_raw`); worker
machines (auto-par, threads) run bytecode.

### Semantics kept equal to the VM

| Operation | Native |
|---|---|
| int `+ - *` | `sadd/ssub/smul_overflow`, bail on overflow |
| int `/` | bail on `b == 0` and `MIN / -1`; `sdiv` |
| int `%` | bail on `b == 0`; `x % -1` is `0`; `srem` |
| `<< >>` | bail unless `0 <= b < 64`; `ishl` / `sshr` |
| float `% **` | Rust `%` / `powf` through a helper |
| float `!=` | true when unordered (Rust `!=`), `==` / `<` ordered |
| `float as int` | `fcvt_to_sint_sat` (Rust `as`: saturating, NaN is 0) |

## Where the pieces are

| Piece | Place |
|---|---|
| MIR capture for a backend | `compiler/src/mir/native_capture.rs`, called from `IlModule::optimize_and_flatten`; exported as `compiler::native_mir` |
| Lowering MIR → Cranelift, JIT driver | `coil-native/src/lib.rs` |
| Native table, `NativeCtx`, bail | `machine/src/native.rs`, `Machine::try_native_call` |
| `--jit` / `--no-jit` | `coil-args` (`NurseryFlag`), `coil compile --jit` in `src/cli.rs`, `src/main.rs` `install_native_bodies` |
| Archive section | `coil-native/src/section.rs`, `ArchivedProgram::native_mir` |
| Tests | `tests/native_jit_cli.rs` (JIT output equals VM output) |
