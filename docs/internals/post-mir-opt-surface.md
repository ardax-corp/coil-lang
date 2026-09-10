# Post-MIR opt surface (tree-shake vs reorder)

Short answer to: after A0–B9 / Q1–Q9 / C1–C3, is **tree-shaking** or
**code reordering** a viable next performance win, or is there still
foundational work first?

**Verdict:** neither is a cheap unlocked win. Function-level unused elim
and heuristic BB layout already ship. Remaining runtime is still
**interpreter dispatch + leftover fuse-IL bodies**, not I-cache or
dead packages. Prefer denser MIR coverage and cost-gate reconstruct
on majority shapes ([opt-generalization.md](opt-generalization.md) A0).

This note is analysis only. Discard hunches that do not survive the
inventory.

Doctrine: [opt-generalization.md](opt-generalization.md). Refuse tables:
[specialize-refuse.md](specialize-refuse.md). Pass contracts:
[`compiler/src/il/opt/README.md`](../../compiler/src/il/opt/README.md).
Pipeline: [pipeline.md](pipeline.md).

Tip when written: C3 (#402). Cross-check refuse / B0 if those move.

## Inventory (what already touches dead / layout)

### Whole-program / package reachability

| Mechanism | What it does | Not |
|-----------|---------------|-----|
| Use-graph discovery | Only `use`/`mod` files are compiled / indexed. A sibling `unused.hy` is never typechecked | Not a second linker; an **imported** module still emits every `fn` in that file |
| `prune_unused_functions` | After link, keep bodies reachable from `main` (and `test_cases` when `--include-tests`) via `Entry` / entry `Jump` / call-like `Byte`. Drops eager builtin dict thunks and unreferenced user fns / methods | Does not run when there is no `main` (snippets keep bodies) |
| `strip_test_declarations` | Default compile drops `test("…")` before emit | `--include-tests` / `coil test` keep them as roots |
| Monomorphize | Clones on **used** call sites (cap → shared generic body) | No unused instantiations to shake |
| Trait dicts | `CodePtr` slots keep **all** flattened methods of a used instance live | Unused dict methods cannot shake while the dict is built |
| `.hyc` / `coil-embed` | Archive is the shaken bytecode. `coil package` concatenates that onto a **full** VM runner | Does not subset VM opcodes; unused `HostInvoke` handlers stay in the binary |

Class / package “dropping” is this graph, not a class-GC pass. Unused
methods of a never-called class die with the function prune. A used
`new C` does not keep unused methods unless something `CALL`s them or
a dict/`CodePtr` names them.

### Intra-function DCE / dead arms

Stack IL (Basic+): `jump_thread` → `dead_block` (ops after JMP/RETURN
until a label) → `stack_dce` → `mem_fwd` / `copy_prop` / `dest_prop` /
`dead_store`. InstCombine folds const-cond `JMPF`/`JMPT` and known-tag
`EQ`; `dead_block` then drops the untaken arm. Pair-match payload
identity is a peep, not a specialize-only dead-arm pass.

MIR: CSE / InstCombine / DestProp / LICM / IV SR each run
`mir::cse::dce` on unused SSA. Dense / LIR emit skip dests the convoy
plan does not need. Unreachable MIR blocks are not a separate layout
pass; leftover `Terminator::Unreachable` is emit-time.

Specialize does **not** invent a second dead-arm eliminator. Const
match arms die the same way as other const branches.

### Layout / “scheduling”

| Pass | Default | What it is |
|------|---------|------------|
| `branch_optimization` (COI-128) | on | Invert a terminating then-arm after `JMPF`/`JMPT` (Known SP). Heuristic: then-arm is cold |
| `block_reordering` (COI-129) | on | Sink **detached jump-only terminating** blocks to the end. Fall-through stays adjacent; labels and polarity **do not** rewrite |
| `invert_guard_branch` | on | `JMPF A; JMP B; A:` → `JMPT B` (fuse `*Jmpt`) |
| Convoy / `tos_carry` / fuse-select | on | Interpreter schedule: keep TOS / fuse windows. This is the real “schedule” |
| PGO / `BranchProfile` | **gone** (#301) | Do not revive |
| `iterative_optimization` | **off** | Extra DCE rounds are a knob, not production |
| `seek_back_edge` | off on Standard | Layout-ish `Seek` on latches; measured fuse loss |

MIR→LIR / dense emit walks `func.blocks` in **BlockId / Braun insert
order**, not RPO / hot-trace. CSE uses RPO for numbering only. There is
no instruction scheduler, no I-cache trace layout, no post-lower
peephole (`adjust_target` is refused).

LICM + integer SR exist on **both** IL and MIR. Specialize widening is
entry + cost gate, not a layout pass.

## Verdicts

### Tree-shaking (whole-program / package unused elim)

**Already mostly there** for what moves `.hyc` size and unused `CALL`
targets.

A second shaker (unused class metadata, unused const-pool strings,
unused VM natives in `coil-embed`) is **low ROI vs denser MIR
coverage**. Flagships and natural suites already call their hot
functions; shaking them does not densify `binary_trees` or format
`main`.

**Not blocked on missing infra.** The graph (`Entry` + use-graph) is
enough. Gaps are small: dicts pin whole instances; imported modules
pay compile of unused exports until prune; const-pool / debug strings
are not pruned.

**Do not** treat “package unused elim” as a new A0 ticket. Userland
packages live in other repos; this compiler never sees unused
coil-stdlib files.

### Code reordering (BB layout, I-cache, schedule)

**Already mostly there** at the only level that pays on this VM:
heuristic cold-arm invert + sink jump-only terminators + fuse/convoy
TOS.

A serious trace / I-cache / list scheduler is **low ROI vs denser MIR
coverage**, and would fight fuse-select (operand windows, Known SP,
label barriers). The machine is a **bytecode interpreter**; shrinking
op count and staying dense beats shuffling bytes for the host I-cache.

**Blocked** only if the hunch is “LLVM-style BB layout after MIR SSA”:
emit order is not a CFG layout IR, PGO is gone, and reconstruct must
still beat fuse-IL on the cost gate. That is missing infra **on
purpose**.

MIR 3-address `DenseBin` does **not** unlock a cheap scheduler. Dense
already assigned regs; remaining tax is `Seek`, Value-ABI CALL edges,
and bodies that still lose the gate or never lift.

## Discard these hunches

- **“SSA MIR means we should reorder blocks now.”** Emit is still
  interpreter IL. BlockId order is fine. COI-128/129 already moved the
  cold returns that matter.
- **“Tree-shake packages like webpack.”** Unused files are off the
  use-graph. Unused fns after `use` already prune. That is not the
  mandelbrot / nsieve / trees gap.
- **“Shake unused opcodes out of coil-embed.”** AOT subset of the VM
  is a product split, not an opt on user `.hy`. Archive minor still
  appends opcodes for everyone.
- **“More DCE after specialize.”** IL DCE + MIR DCE already run.
  Specialize dead arms are const-cond + `dead_block`.
- **“PGO layout now that coverage is wider.”** Explicit non-goal
  (A0, #301). Heat knobs stay dead.
- **“Iterative IL until fixpoint.”** Off for a reason; not a majority
  win.

If a hunch here later has numbers on a **natural** body (A4), revive
it with embed A/B — not with a vanity layout microbench.

## Next wins the recent work actually unlocks

Ranked for **majority programs** + **checksum + cost gate**. Not a
wishlist. Parked islands stay named so they are not confused with
“cheap now.”

| Rank | Win | Why this, not shake/reorder | A0 |
|------|------|------------------------------|----|
| 1 | **Cost-gate keep-rate** on bodies that already lift | Q6–Q8 / B2–B7 / C1–C2 opened entry; many still lose on `Seek` / boxed reconstruct. Same spirit as B2 convoy and S2l: keep only when ≤ fuse | Majority of *eligible* helpers; no new opcode |
| 2 | **Leftover unmapped alloc / class `new` maps** | B6 mapped `ArrayPush` + CALL+`Make*`. Unmapped grow / `InitTyped` still fuse-IL. Maps let SROA/LICM delete work; layout does not | Heap loops people write; cost gate still refuses boxed |
| 3 | **Dense-native remaining heap ops** (A2 leftover) | Index/len/make/push are native; leftover class field / grow edges still box in-loop | Prefer dense ops over residual LOAD/STORE |
| 4 | **Q9 format vs whole-`main` fuse** | R3 maps `FORMAT`; dense infer still refuses table ops. `for_in_sum` `main` stays fuse because format + `Vec.push`. Splitting / densifying the numeric callee already happened; the `main` tax is I4/Q9, not layout | Lots of programs print; do not add a second Format lowering |
| 5 | **Boxed multi-payload match** | Hard wall. `binary_trees` `item_check` stays fuse-IL for this, not because of unused fns | Flagship + real enums; **foundational**, not a shake |
| — | Parked | User `Iterator` / coro / dict `for`; residual `Byte`/`Pow`/bitwise; `CALL` width > 2; P5 Cranelift; PGO | Later island or explicit non-goal |

Do **not** rank tree-shake or BB reorder in this list. They are
maintenance of what exists, not the unlocked surface.

## Available now | Needs work | Don't bother yet

### Available now

- Function reachability prune (`prune_unused_functions`) + use-graph
  file discovery + test stripping
- IL DCE family + InstCombine const arms + MIR SSA DCE
- Heuristic branch invert + sink jump-only terminators
- Fuse-select / convoy / `tos_carry` as the interpreter schedule
- MIR default for eligible bodies; fuse-IL fallback; cost gate
- Dense-native heap ops where maps exist; Q6 counted `for`; Q7/Q8
  CALL/match ladders; B3/B7/C1 two-slot; Q9 LIR strings + R2 bytes +
  R3 format maps

### Needs work (foundational / cost, not layout)

- Reconstruct quality so lifted bodies **keep** (Seek / ABI edges)
- Unmapped class/`new` / leftover grow maps
- Multi-payload `Unpack` / `JumpIfMatch` arity > 1 (trees-shaped)
- Dense still refusing table ops (format `main`)
- N>2 `CALL`/`RETURN` (archive encoding) — only if majority needs it
- User `Iterator` (later Q6) — not majority until that surface exists

### Don't bother yet

- A second whole-program tree-shaker or class unused-elim pass
- I-cache / trace BB layout, MIR block RPO emit, instruction scheduling
- PGO / heat-guided invert
- Subsetting `coil-embed` opcodes per program
- Fuse-only peeps whose job is to beat a dense reconstruct
- Cranelift P5 as the next AOT harvest

## How to read a future “we should shake / reorder” PR

A4: prefer `coil-embed`; natural suites; flagships flat or identical
archives; no env toggles. If the PR only shrinks `.hyc` and does not
change a claimed body’s wall time, it is a **size** change — say so.
If flagship archives are identical, it did not touch the hot path.
