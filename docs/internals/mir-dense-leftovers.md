# Denser MIR leftovers (post A0–C3)

Ranked leftovers after A0–B9 / Q1–Q9 / C1–C3. **Not** tree-shake, IPA
shapes, or BB layout — those are a separate surface
([#405](https://github.com/ardax-corp/coil-lang/pull/405)). This note is
the denser-coverage kick list for majority programs + checksum + cost
gate ([opt-generalization.md](opt-generalization.md) A0).

Tip: D0 [COI-354](https://linear.app/ardax/issue/COI-354) (`7960eed2`).
D1 [COI-355](https://linear.app/ardax/issue/COI-355) maps class `new` /
field edges (this PR).

## Cross-check (open Linear / landed PRs)

| Ticket | Status | What actually landed | Leftover |
|-------|--------|----------------------|----------|
| [COI-351](https://linear.app/ardax/issue/COI-351) C2 | **Done** (#401) | Numeric free-fn Range param / two-slot `CALL`/`RETURN`; counted `for` without `GetField` | User `Iterator` / coro / dict / heap-field Range → [COI-353](https://linear.app/ardax/issue/COI-353) **C2b** (Todo). Not this wave. |
| [COI-350](https://linear.app/ardax/issue/COI-350) C3 | **Done** (#402) | Compiler-internal `DraftDeoptMap`, named-let remap, sparse emit locs | P5 resume, incomplete convoy maps, per-PC locals. Debugger, not a hot-path densify. |
| [COI-344](https://linear.app/ardax/issue/COI-344) B6 | **Done** (#396) | Mapped `ArrayPush` / `DenseArrayPush` (archive **4.11**); CALL+`Make*` / `InitTyped` drafts bind | Unmapped **class** edges were D1; `DenseMake` has no Object kind; `item_check` still fuse (match wall, not maps) |
| [COI-335](https://linear.app/ardax/issue/COI-335) A2 | **Done** (#379) | `DenseIndex` / `DenseStoreIndex` / `DenseArrayLen` / `DenseMake` / `DensePush` | No dense field op; Object `Alloc` reconstructs `InitTyped` — **D2** |
| [COI-340](https://linear.app/ardax/issue/COI-340) B2 | **Done** (#392) | Self-`CALL` convoy so tight `fib`/`tak` can win the gate | Other lifted bodies may still lose on Seek / boxed reconstruct |
| [COI-354](https://linear.app/ardax/issue/COI-354) D0 | **Done** (#407) | `slot_env` follows trivial-phi subst so in-loop `ArrayPush` + pin loops verify; `nsieve` keeps dense-native | Straight-line / select still lose on `emit_cost`; multi-payload match stays later |
| [COI-355](https://linear.app/ardax/issue/COI-355) D1 | **Done** (this PR) | Sound GC maps across `InitTyped`+`SetField`/`GetField` on escaping named objects | Dense field / Object `DenseMake` → [COI-356](https://linear.app/ardax/issue/COI-356) **D2**. Escaping `self` stays boxed-once (Q2) |
| [COI-347](https://linear.app/ardax/issue/COI-347) B9 | **Done** (#399) | Q9 R3 maps `FORMAT`/`STRINGIFY` | Dense infer still refuses table ops. Unicode/regex → [COI-348](https://linear.app/ardax/issue/COI-348) **B10** |
| [COI-352](https://linear.app/ardax/issue/COI-352) C1b | Todo | N>2 modeled `CALL`/`RETURN` | Archive encoding. Not majority until that ABI exists. |

No Linear ticket today for leftover dense field ops / boxed multi-payload
match. D1 class-`new` maps is [COI-355](https://linear.app/ardax/issue/COI-355).

## Inventory (#405 ranking, verified on tip)

### 1. Cost-gate keep-rate (B2 / S2l spirit)

**Status:** **landed** for `nsieve` ([COI-354](https://linear.app/ardax/issue/COI-354)).
B2 remains the self-`CALL` convoy; D0 is reconstruct hygiene so a body that
already lifts can pass SSA verify and keep native heap ops.

**What is true:** Infer already lifts more than emit keeps. `try_specialize_body`
opts SSA, then refuses when:

- residual in-loop `Make*` is still stack-boxed (`residual_heap_box`)
- straight-line / select reconstruct `emit_cost` > fuse (`Seek` weights 2 + frame/16; `LOAD`/`StorePop` cost 2)
- loops skip that static compare unless they are a select diamond or still have in-loop Make*
- two-slot reconstruct grew `MakeEnum`
- SSA verify fails (was: trivial-phi deletion left dead ids in `slot_env` → `GcBarrier` roots)

B2 (#392) made tight `fib`/`tak` convoy CALL on the stack so the gate can
keep them. Skipping the gate was a ~2× fib lose — do not revive that.

**`nsieve`:** maps already bound (B6). Lower then died on
`vN uses undefined vM` because `ArrayPin` / loop header phis were removed as
trivial while `slot_env` still named the old dest. `rewrite_subst` now maps
`slot_env`; `fill_live_roots` ignores undefined ids. `s3_nsieve_index_store_or_fuse_il`
requires `DenseBin` + native Index/StoreIndex. Cost gate unchanged.

**Leftover:** other lift-then-lose helpers (straight-line Seek tax, boxed
`InitTyped`). `s2d_inloop_escape.hy` stays fuse when reconstruct is boxed (S2l).

**Perf surface:** `examples/perf/nsieve.hy` (kept). Flagships `fib`/`tak` are
controls (archives should stay identical).

**Effort:** further keep-rate is still one body at a time. No new opcode, no
skip-the-gate.

### 2. Leftover unmapped alloc / class `new` maps

**Status:** **landed** ([COI-355](https://linear.app/ardax/issue/COI-355)).
B6 majority grow/Make* plus D1 class/field maps.

**What is true:** `refuses_alloc` covers `Make*` / `InitTyped` / `ArrayPush`
/ `FORMAT`/`STRINGIFY`. S2b drafts bind those when infer types the body
(B6 typed one-word `CALL`; D1 types heap GetField/SetField/LoadField).
`binary_trees` `bottom_up` MakeEnum can map; `item_check` is refused for
**arity-2 match**, not missing maps.

**Hunch that does not hold:** “Unmapped alloc still means `Vec.push`.”
`Vec.push` is a mapped grow safepoint. Field ops are still
`LirRefuse::HeapField` for **reconstruct** (D2). Maps now bind across
`InitTyped`+field. `MirAllocKind::Object` still cannot `DenseMake`.

I3 already unboxes **non-escaping** named `new C`. Escaping `self` /
`take(p)` stays heap — Q2 identity, not a map bug. D1 does not SROA
escaped `self`.

**Blocker for keep:** Object alloc has no native kind; LIR still refuses
HeapField. Maps first (this ticket); native reconstruct is D2 so the
cost gate can keep.

**Perf surface:** `examples/perf/field_hot.hy` (maps bind; body stays
fuse-IL). Escaping `new` + `take`. Not `nsieve`. Not trees walk.

**Effort:** done. D2 is medium + archive minor if a new dense field op.

### 3. Dense-native leftover heap / class edges (A2 leftover)

**Status:** open gap after #379.

**Landed native:** `DenseIndex`, `DenseStoreIndex`, `DenseArrayLen`,
`DenseMake` (array/tuple/enum tag), `DensePush` (CALL/HostInvoke),
`DenseArrayPush` (B6). Dense emit **refuses** `FieldLoad`/`FieldStore`
(“I3 is MIR→LIR”).

**Hunch that does not hold:** “I3 field-SROA will densify `field_hot`.”
`field_hot` calls methods on a live object (`self` identity). Q2 boxes
once. I3 is non-escaping locals only. `perf_field_hot_reuses_repeated_string_keys`
still requires `GetField`.

**Blocker:** no dense field opcode / Object `DenseMake`; LIR field reconstruct
only for unboxed slots. Coupled to (2): maps first, then native ops so the
cost gate can keep.

**Perf surface:** same as (2); in-loop Index bodies that still residual-box
(A2 already won the mapped Index/`MakeArray` majority — `times_a`,
`indexed_sum`, Q6 `sum`).

**Effort:** medium + **archive minor** if a new dense field op. Prefer
regular field native over a `field_hot` peep.

### 4. Q9 vs whole-`main` fuse

**Status:** ladder **working as designed**, not a missing densify of `main`.

**What is true:** Q9 R1 (#389) reconstructs table `STRING`/`PRINT`/`FORMAT`/
`STRINGIFY` on MIR→LIR. R3 (#399) maps `FORMAT`/`STRINGIFY`. Dense infer
**still refuses** table ops (`refuses_dense_string`) so numeric specialize
is not stalled. `pipeline_format_loop_stays_fuse_il` requires `FORMAT` and
**no** `DenseBin`. `for_in_sum` `sum` is already dense (Q6); `main` is
format + `Vec.push`.

**Hunch that does not hold:** “Printing `main` should take dense so the
program is dense.” A0: fuse-IL is the fallback. Mixing table ops into dense
infer fights numeric islands. LIR keep is the cost gate on format helpers;
whole-`main` fuse is expected.

**Blocker:** none for majority numeric. Densifying table ops is a later
island (and still cost-gated). Unicode/regex is B10, not format-in-`main`.

**Perf surface:** `for_in_sum` `main`, `array_mut` `main`, any `write_all` +
`format` driver. Helpers already split. Flagships that never print stay
identical.

**Effort:** skip this wave. If kicked: LIR keep-rate on format loops only;
**no** second Format lowering; **no** dense table ops.

### 5. Boxed multi-payload match (`binary_trees` `item_check`)

**Status:** hard wall. Unchanged after Q8 (#388) / B6.

**What is true:** `JumpIfMatch` / `Unpack` arity > 1 → `LirRefuse::Match` /
`Unpack`. Q8 densifies niche / two-slot register `Br` (arity ≤ 1). Boxed
unary `JumpIfMatch` may LIR (I2). `item_check` is `Tree::Node(left, right)`
→ one `Unpack` of arity 2. `aot_p3_binary_trees_make_enum_inventory`
requires that Unpack and zero `MakeEnum` on the walk.

**Hunch that does not hold:** “Trees stay fuse because `bottom_up` is
unmapped alloc.” B6 maps CALL+`MakeEnum`. The walk is the match wall.

**Blocker:** per-index payload maps + dense/LIR reconstruct of multi-word
`Unpack`. Foundational island, not a keep-rate tweak.

**Perf surface:** flagship `binary_trees.hy` (`item_check` hot). Other
boxed multi-payload enums.

**Effort:** large (island). Do not sneak a trees-shaped opcode.

## Ranked kick order (Dimitar)

Majority programs + cost gate. One ticket at a time. A4 on every code PR.

| Rank | Kick | Why this rank | Not |
|------|------|---------------|-----|
| **P0** | Keep-rate on bodies that **already lift** | **Landed** [COI-354](https://linear.app/ardax/issue/COI-354): `nsieve` keeps dense. Residual: other Seek/box losers | Skip-the-gate. Vanity microbench. |
| **P1** | Class `new` / field **maps** (B6 leftover) | **Landed** [COI-355](https://linear.app/ardax/issue/COI-355): maps bind on `InitTyped`+field. Escaping identity stays boxed-once (Q2). | Re-doing `ArrayPush` maps. Unboxing escaped `self`. |
| **P2** | Dense-native field / Object `DenseMake` (A2 leftover) | After P1, so reconstruct is native and the gate can keep. | Bench-shaped `GetField` fuse. |
| **P2** | Dense-native field / Object `DenseMake` (A2 leftover) | After P1, so reconstruct is native and the gate can keep. | Bench-shaped `GetField` fuse. |
| **P3** | Boxed multi-payload match | Flagship + real enums. Hard wall. Bigger than P0–P2. | Trees opcode. Forcing dense `main`. |

**Do not kick now:** whole-`main` dense format (4); C2b user Iterator
([COI-353](https://linear.app/ardax/issue/COI-353)); C1b N>2
([COI-352](https://linear.app/ardax/issue/COI-352)); B10 unicode
([COI-348](https://linear.app/ardax/issue/COI-348)); C3 P5 resume;
tree-shake / BB reorder (#405).

## Discard these hunches

- **C2 / C3 are the denser-MIR leftovers.** They shipped (#401 / #402).
  Leftovers are Iterator/coro (language) and P5 (native), not keep-rate.
- **B6 left `ArrayPush` unmapped.** Mapped. Leftover is class field / Object.
- **`nsieve` is dense after B6.** Maps bound; keep needed D0 `slot_env` rewrite.
- **`field_hot` is an I3 miss.** Escaping `self`. Q2.
- **Densify printing `main`.** Helpers are the island. Table ops stay off dense.
- **Trees are an alloc-map miss.** `item_check` is arity-2 match.
- **Skip the cost gate so nsieve keeps.** B2: that path regresses `fib`.

## Recommend (do not file here)

Opt generalization board, same shape as B6/C2 (tip SHA, prove, A4):

1. **P0 — Cost-gate keep-rate after lift (Seek / boxed reconstruct)**  
   **Landed** [COI-354](https://linear.app/ardax/issue/COI-354) for `nsieve`.
   Further bodies: checksum + embed wall ≤ fuse. No skip-the-gate.

2. **P1 — B6 leftover: class `new` / `GetField` maps**  
   **Landed** [COI-355](https://linear.app/ardax/issue/COI-355).
   `InitTyped`+`SetField`/`GetField` on escaping named objects. I3
   non-escaping stays unboxed. Maps bind; cost gate still refuses boxed
   reconstruct.

3. **P2 — A2 leftover: dense-native field / Object make**  
   After P1. Native reconstruct of field load/store (and Object
   `DenseMake` if needed). Archive minor only if a new opcode. Prove
   field loops; flagships flat.

4. **P3 — Boxed multi-payload match (`Unpack` arity > 1)**  
   I2 leftover. `binary_trees` `item_check`. Per-index payload maps.
   Checksum; flagships flat or better. No trees opcode.

## Related notes

- Doctrine: [opt-generalization.md](opt-generalization.md)
- Refuse: [specialize-refuse.md](specialize-refuse.md)
- Islands: [mir-islands.md](mir-islands.md)
- Maps: [mir-stack-maps.md](mir-stack-maps.md)
- S2l tax: [s2d-inloop-make-tax.md](s2d-inloop-make-tax.md)
- Q6 / Q9 ladders: [q6-iterator-protocol.md](q6-iterator-protocol.md),
  [q9-format-string.md](q9-format-string.md)
- Deopt leftover: [mir-deopt.md](mir-deopt.md)
