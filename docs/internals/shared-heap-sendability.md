# Shared-heap sendability (C0 / STW)

[COI-363](https://linear.app/ardax/issue/COI-363/e5-c0-shared-heap-sendability-design-stw)
locks the sendability story for **shared-heap steal**. This note is the
contract [COI-365](https://linear.app/ardax/issue/COI-365/e6-c1-shared-heap-loop-chunk-steal-stw)
implements.

**C1 status (E6):** counted-loop chunks use HostInvoke `thread_spawn_shared`
(**137**, archive **minor 15**) on Layer A epoch STW. User `thread::spawn`
stays isolate + `PortableValue`. Expression IPA (E7) is not this ticket.
Do **not** merge [#403](https://github.com/ardax-corp/coil-lang/pull/403).

Related: [auto-par.md](auto-par.md), [gc-incremental.md](gc-incremental.md),
[mir-stack-maps.md](mir-stack-maps.md), [heap-identity.md](heap-identity.md),
[io-reactor.md](io-reactor.md). Findings that ranked the ladder:
PR [#403](https://github.com/ardax-corp/coil-lang/pull/403) (do **not** merge
that PR; use the writeup only). Isolate-tax cuts that this note builds on:
[COI-360](https://linear.app/ardax/issue/COI-360) E2 (#413). Maps that make
multi-stack GC honest: [COI-359](https://linear.app/ardax/issue/COI-359) E1
(archive minor 14).

## Goal

True steal means several reactor mutators run Coil bytecode against **one**
[`Heap`](../../machine/src/memory/heap.rs) and **several** operand stacks.
Crossing a job boundary must not deep-copy the live heap graph through
[`PortableValue`](../../machine/src/thread.rs).

C0 answers four questions:

1. Which values may appear as live roots on more than one stack at once
   (**share whitelist**).
2. What a mutator may do to a shared object (**freeze / disjoint write / refuse**).
3. How GC sees every stack (**STW safepoints**; **maps mandatory**).
4. Where stacks live (**TLS stacks on that one Heap** — not a private Heap
   per help steal).

## Non-goals (this note and C1)

- Concurrent / on-the-side mark while mutators run. [gc-incremental.md](gc-incremental.md)
  already defers “concurrent mark on OS worker threads”. C0 does **not** lift
  that. Incremental S4 stays a **single-mutator** path.
- Moving / compacting GC. Objects still do not move
  ([heap-identity.md](heap-identity.md)). Mapped-slot relocate stays identity.
- Treating `COIL_AUTO_PAR` as a runtime `.hyc` switch.
- Dropping isolates for user `thread::spawn` / channels in C1. User spawn
  keeps today’s `PortableValue` copy + `NotSendable`. Shared-heap is an
  **auto-par / loop-chunk** path first.
- Merging #403.
- A language-level `Send` trait or new opcodes.

## Today (isolate) — what C replaces

CPU jobs are [`reactor::Job`](../../machine/src/reactor.rs): isolated
`call_function` on a worker `Machine`. Args and results cross via
`value_to_portable` / `portable_to_value`. Cycles, streams, threads,
coroutines, `Fn` / `PolyFn`, libraries, `Root`, and `Weak` return
`ThreadErrorTag::NotSendable`. Channel / lock objects share the host
`Arc` and re-wrap a fresh heap object on the child.

E2 (on `main`) already cut isolate tax **without** sharing a Heap:

| Piece | Behavior |
|---|---|
| Bytecode | `load_shared_program` pins `Arc` code / constants / strings |
| Join-help | TLS `HELP_VMS` checkout instead of `Box::new` per steal |
| After job | `reset_isolate_heap`: drop frames, collect or unmap extra 64KiB slabs |

Each helper / pool worker still **owns** a `Heap`. Join-help nested on
`COIL_MAX_WORKER_THREADS=1` is several Machines on one OS thread, each with
its own slab. That is the RSS cliff C exists to remove.

Typecheck `is_thread_sendable_ty` is the **copy** gate (immediates, strings,
aggregates of those, `Sender` / `Receiver` / `Mutex` / `RwLock`). It is not
a share-by-pointer gate. C0 does not widen user `spawn` types.

## Target picture

```
          ┌─────────────────────────────────────────┐
          │  Heap (one slab, one intern table,      │
          │  immortal unit enums, one GC epoch)   │
          └──────────────┬──────────────────────────┘
                         │  alloc + STW collect
     ┌───────────────────┼───────────────────┐
     ▼                   ▼                   ▼
 root Machine      pool worker           TLS help stack
 (frames+stack)    (frames+stack)        (frames+stack)
```

- **One Heap** per root `Machine` / reactor world (same lifetime as today’s
  isolate root, not per job).
- **N mutator stacks**: root + each pool worker that is inside a shared-heap
  job + each nested TLS help stack. Stacks are `Machine` frames / operand
  storage / `frame_pins` only.
- A stolen job receives **raw `Value`s** that are either immediates or
  pointers into that Heap. It returns the same. No `PortableValue` graph
  walk on the hot path.
- Isolate + `PortableValue` remains the **fallback** when the share proof
  fails (spawn `Err` / sequential arm, same as today’s join miss).

## Share whitelist

A value is **shareable** when every mutator that holds it can treat the bits
as a `Copy` `Value` without cloning payload memory, and without racing on
object identity.

| Class | Examples | Why shareable |
|---|---|---|
| Immediate | `int` / `float` / `bool` / `byte` / `unit` / null / two-slot ABI words that are not heap addrs (`is_immediate_value`) | No object; bits are the value |
| Immortal | Arity-0 unit enums (`Heap::immortal_unit_enum`) | Never swept; address-stable for the Heap lifetime |
| Arc handles | `Sender` / `Receiver` / `Mutex` / `RwLock` **objects already on this Heap** | Host `Arc` is already `Send`; the Coil wrapper stays one object |
| Frozen | `readonly` graphs the compiler proved will not be written this epoch; interned **program** strings the mutators only read | No writer → no data race; GC still roots them |
| Region (optional later) | Epoch bump / nursery discarded at join | Dead before the next STW; not a cross-epoch root |

**Not shareable** (same refuse set as `encode_value`, plus mutable alias):

`Stream`, `Thread`, `Coroutine`, `Fn`, `PolyFn`, `Library`, `Root`, `Weak`,
cyclic heap graphs, any object another mutator may grow or field-store
without a disjoint-write proof.

C1 does **not** need to share `Fn` objects: loop-chunk workers already take an
entry PC (`Job.entry`) plus immediates / array pointers, same as AlwaysPar
`MakeFn` + spawn today.

Interned strings are **not** immortal. The intern table is a cache; unmarked
literals are swept and `program_string_cache` is not a GC root
(`Machine::gc_collect`). Sharing a `RefString` pointer is allowed only while
some stack or frozen graph roots it. Do not treat intern identity as a
lifetime.

## Mutator contract

Exactly one of the following holds for each shared object for the duration of
a steal epoch. Mixing them on one object is a refuse.

### Freeze

- Proof: `readonly` on the value **and** no write in any stolen body
  (purity already refuses index/field stores in loop IPA).
- Runtime: no `StoreIndex` / `SetField` / grow / `vec` host mutation on that
  object until `end_steal`.
- Any mutator may **read** (including `ArrayPin` / `IndexPin*` on a frozen
  array). Pins cache `Object` (`Copy` `Gc`); that is valid because objects
  do not move.
- Optional defense (open): a header frozen bit that panics on write. C1
  may skip the bit if the compiler proof is closed and the chunk worker is
  a compiler-emitted body.

### Disjoint write

- Proof: partitioned index ranges (or disjoint fields) such that no two
  mutators store the same slot, and **length is stable** (no `ArrayPush` /
  `DenseArrayPush` / host grow).
- C1 shape: loop chunks `[lo, hi)` that only store `out[i]` for `i` in that
  half-open range, plus a **private** reduction partial (never a shared
  `acc` location). This is the independence table in
  [auto-par.md](auto-par.md) lifted from isolate copy to in-place stores.
- Element stores are pointer-sized `Value` writes to disjoint indices.
  That is the data-race story: no tearing, no overlapping stores, no
  header mutation except at STW.
- Alias: if `in` and `out` may be the same array, refuse unless the chunk
  is read-only or the ranges still do not overlap a live read of a slot
  another mutator writes. C1 default: **refuse alias**; sequential fallback.

### Refuse

Everything else: isolate job + `PortableValue` (or sequential codegen).
Wrong proofs must not “best effort” share. `NotSendable` stays the runtime
tag for the isolate path; shared-heap refuse is a **compiler / spawn
gate**, not a new enum case, unless E6 finds it needs a distinct log.

Private allocations made **by** a stolen job (temps, boxed partials) are
owned by the shared Heap and rooted only on that mutator’s stack until
published at join. They are not freeze and not disjoint-write of a
pre-existing object. They become ordinary heap objects; STW must scan that
stack. A later **region** bump that dies at join may elide them from the
next mark — not required for C1 int reduce.

## Multi-worker GC handshake

### Why maps are mandatory

S2b maps persist on `.hyc` / embed (archive **minor 14**, E1). Shared-heap
steal of heap pointers is allowed only when `ThreadProgram.stack_maps` is
**non-empty and real** (`has_real_maps` / `wire_thread_program_with_maps`).
Layer A C1 **immediate-only** loop chunks may steal without maps because
collect is forbidden in-epoch (abort instead). Pre-14 archives and unmapped
**allocating** bodies stay **isolate**.

Conservative operand-stack scanning on one mutator is the current fallback
when maps are empty. With several mutators it is not a contract: a stolen
numeric kernel can leave immediate bit-patterns that look like heap addrs,
and C must not keep a second root story. Maps plus STW are the one story.

`ArrayPin` tables are extra roots (`collect_vm_root_addrs` already walks
`frame_pins`). Handshake must visit **every** mutator’s pins, frames,
operand stack, statics, resume/coro stacks, and mapped slots.

### Incremental S4 vs steal

Today mark finishes at the **alloc** safepoint so a single mutator never
runs in `GcPhase::Marking`. Opcode `SetField` / `StoreIndex` skip SATB
because of that.

C0 rule: **no mutator runs while the shared Heap is Marking or Sweeping.**

Before `begin_steal`:

1. Finish any in-flight incremental cycle on the root (`gc_collect` drain,
   same as explicit `gc::collect`).
2. Raise / ignore the incremental threshold for the epoch **or** switch the
   Heap to epoch-STW (below). Do not start a new incremental slice on a
   worker.

SATB is unused during the epoch because mark does not overlap mutators.
Host vec mutations in a stolen body are already a purity refuse.

### Two STW layers (C1 uses the first)

**Layer A — epoch STW (C1 default).** No collect between `begin_steal` and
`end_steal`. Allocations may bump `alloc_bytes` and map extra slabs. If a
chunk would collect (`should_collect` / explicit `gc::collect` / OOM):
**abort the epoch** (sequential leftover or isolate fallback). After join,
the joiner is the only mutator and runs a normal collect with all published
roots on its stack.

This is enough for C1 counted int-reduce / disjoint stores that allocate
little. It avoids handshake latency on the first client.

**Layer B — handshake STW (C0 contract, needed when a steal may collect).**
Cooperative stop:

1. A mutator that must collect sets `gc_requested` and becomes **leader**.
2. Every shared-heap mutator arrives at a **safepoint** (below), increments
   `arrived`, and waits.
3. When `arrived == mutator_count`, the leader concatenates roots from every
   stack (maps + pins + immortals) and runs one mark-sweep (existing
   `gc_collect` logic, roots generalized). Finalizers run on the **leader**
   with other mutators still stopped. Weaks clear at mark→sweep as today.
4. Release; `gc_requested` clear; mutators resume.

`mutator_count` is the number of stacks in the epoch (root + workers +
nested help), not the OS pool size.

### Safepoint set for Layer B

Shared-heap mutators must be able to stop without waiting for an alloc:

| Site | C1 |
|---|---|
| Heap alloc (existing `gc_safepoint`) | Yes |
| Explicit `gc::collect` | Refuse in stolen bodies; if it happens, abort epoch |
| Job entry / exit / join | Yes (natural) |
| Counted-loop back edge | **Required for Layer B** if the body can run long without alloc. Layer A C1 may omit the poll if collect is forbidden in-epoch |
| HostInvoke | Only if a shared-heap body can reach one; C1 chunks should not |

A Layer B implementation that forgets back-edge polls will deadlock when
one chunk allocates into a collect while another is in a tight `i += 1`
loop. That is why C1 starts on Layer A.

Parked IO (`wait_fd_helping` / `Stream.park`) must **not** sit inside a
shared-heap epoch. Stolen C1 bodies are CPU-pure. Handshake vs IO help-steal
is an open (refuse for C1).

## TLS stacks on one Heap (vs E2)

E2’s TLS helper is the right **checkout** shape and the wrong **ownership**
shape for C.

| | E2 isolate (now) | C shared heap |
|---|---|---|
| Checkout | `HELP_VMS` pop / push, truncate idle to 1 | Same TLS pool of **stack** Machines |
| Heap | Each Machine owns one; `reset_isolate_heap` unmaps extra slabs | Heap is **not** on the helper; borrow the epoch Heap |
| After job | Collect or replace `Heap` | Drop frames / pins / seek 0; **do not** unmap the shared slab |
| Nested steal | Extra helper Machines, each with a Heap | Extra stacks, still one Heap |
| Pool worker loop | `worker_loop` keeps one Machine + Heap for sequential isolate jobs | Isolate jobs keep today’s private Heap **or** bind the shared Heap only while a C job runs |

C1 may keep isolate Machines for ordinary `thread::spawn` jobs and only
bind the shared Heap for proven loop chunks. Do not reset the root Heap
between C jobs.

`HostStateGuard` / print redirects stay per-stack (E2 already saves and
restores them around help). They are not Heap state.

Static slots: one `statics` array on the **root** (or Heap-side table).
Worker stacks must not each `init_static_slots` into a private copy that
then aliases interned pointers from another Heap. C1 loop workers should
not touch statics; if they need a string literal, they use the shared intern
table / program string cache **on the Heap**, rooted for the epoch.

## E6 (C1) — implement against this

First client: **B’s loop chunks** (E4 grain, or today’s `while` loop IPA) as
stolen jobs on one Heap.

Eligibility (all must hold):

1. `ThreadProgram` has real S2b maps (E1).
2. Body matches loop IPA independence / purity / int reduction **or**
   disjoint `out[i]` stores with stable length.
3. Args are whitelist: induction bounds (immediates), frozen inputs,
   disjoint `out`, Arc handles only if the body does not need them (C1
   should not need channels).
4. No `gc::collect`, grow, yield, FFI, spawn, IO park in the worker.
5. Combine is associative fold of **private** partials on the joiner after
   join (same `ADD` / `MUL` as today).

Runtime sketch (C1 / E6):

1. `begin_steal` on the root Heap; register mutator stacks as they enter.
2. Submit a job that carries `Value` args (`SpawnArg::Shared`). Skip
   `value_to_portable` for those args.
3. Worker `call_function` on a TLS stack bound to the Heap.
4. Store an immediate (or shared pointer bits) into `JoinState` without
   encoding a graph.
5. `end_steal`; joiner folds; Layer A collect if needed.

Prove C1 with counted-loop boards, checksums vs sequential, RSS vs isolate
IPA, A4. Flagships may not move; hit benches may.

Failure: any gate miss → today’s isolate spawn or sequential worker call.

## E7 (C2) preview

Expression IPA without deep copy uses the same whitelist. Fib-style arms
that return immediates share nothing. Arms that allocate a small graph must
either freeze it before the sibling reads it, or allocate into the shared
Heap and publish the pointer at join (still one Heap, no copy). Nested
AlwaysPar remains an E3 shape issue, not a sendability issue.

## Open questions (Architect)

Mark **Q** items that E6 must not guess silently.

1. **Layer A vs Layer B for C1.** This note picks Layer A (no in-epoch
   collect). Discard if C1 must GC while chunks run — then back-edge polls
   are mandatory before steal ships.
2. **Runtime freeze bit** vs type-only `readonly` / purity. Defense in depth
   vs hot-path header checks.
3. **Heap lock.** Mutex around `Heap::alloc` + collect is enough for C1 if
   disjoint stores never take the lock. Per-size-class locks are later.
   `Gc::payload_mut` stays a clippy deny — disjoint element writes should
   use the existing `StoreIndex*` paths under the proof, not a new
   unsound `payload_mut` story.
4. **User `thread::spawn`.** Stay isolate until a later ticket, or allow
   whitelist share on explicit spawn in C1? Recommendation: isolate.
5. **Region bump.** Needed for C2 temps? C1 int reduce can allocate nothing.
6. **Finalizers.** Leader-only during Layer B; Layer A C1 should refuse
   `fn drop` types in stolen graphs (loop IPA already has no instances).
7. **Debugger.** `coil debug` during a steal epoch: refuse attach, or STW
   and debug the leader? Recommendation: refuse / sequential under debug.
8. **`COIL_MAX_WORKER_THREADS=1`.** Nested TLS stacks on one Heap must not
   deadlock Layer B (the joining stack is a mutator and must count as
   arrived when it help-steals). Layer A avoids this.
9. **Static / intern races.** Confirm C1 workers never intern without the
   Heap lock.
10. **Pin across join.** A pin table is per-stack and dies at job reset.
    Do not persist coro pins (already refused in heap-identity).

## Refuse / later

- Concurrent multi-mutator GC (true parallel mark).
- Sharing `Fn` / coroutine / stream graphs.
- Growing a shared array during steal.
- Conservative-stack shared-heap GC.
- Starting the reactor at `Machine` construct.
- Per-help private Heap “just for temps” — that is E2 isolate, not C.
