# Userland locks + parallelization hints (F3)

[COI-369](https://linear.app/ardax/issue/COI-369/f3-userland-locks-parallelization-hints-call-bag-escapes).
Architect-locked model: **userland locks + hints**, not compiler-inserted
locks. This note is the first-cut contract. **Discard freely** if the
wording, coverage rule, or sequential-on-cover choice is wrong.

Related: [auto-par.md](auto-par.md) (F0–F2 IPA),
[shared-heap-sendability.md](shared-heap-sendability.md) (C0 refuse).

## Goal

Heap objects / user classes stay unshared by default (C0). Unlocked FD /
FFI escapes refuse auto-par the same way. When a **call bag** (independent
IPA arms: `f(…) ⊕ g(…)`, n-ary associative `+`/`*`/`^`, let-bound arms)
would fork except for a shared escape, compile-time **info** names the
edge: lock resource `R` across the parallel region. The compiler does **not**
insert a lock or auto-fork on the hint. A covering userland lock is a
**coverage check** this cut; admit of shared steal is a later hold check /
runtime assert. Missing lock → today’s sequential refuse.

Out of scope: compiler-owned per-FD locks; silent I/O reordering without a
user lock; claiming deadlock freedom; boiling every FFI type.

## Hint text

Diagnostic **E0805** (`ParLockHint`), severity **info** (not an error).
Emitted only when `COIL_AUTO_PAR` is on (same as IPA codegen).

Unlocked (the user has not taken a covering lock):

```
this call bag in `{fn}` would be parallelizable if resource `{R}` ({kind}) is locked across the parallel region
```

`kind` is one of `FD` / `FFI handle` / `mutex` / `user object`. Help:

```
take a covering lock in userland (e.g. `thread::with_lock`); the compiler does not insert locks or auto-fork on this hint
```

Covering (`with_lock` / `with_read` / `with_write` callback contains the bag):

```
covering lock on `{R}` (mutex) around the call bag in `{fn}`; shared steal stays sequential this cut (missing lock remains today's refuse)
```

Help:

```
the compiler does not auto-fork on a covering lock yet; a later hold check / runtime assert may admit shared steal
```

No hint when:

- The function is **pure** (F0–F2 already IPA or grain-refuses).
- Impurity is not lockable (`panic` / `yield` / clocks / unknown).
- There is no call-bag shape (a single `write` in `main` is not a bag).

Loop IPA with an impure body is **not** hinted this cut (expression call
bags only). Grain is **not** required for a hint: the refuse reason is the
escape, not `W`.

## Coverage rule

A lock **covers** a bag when a `thread::with_lock` (or rwlock
`with_read` / `with_write`) call’s callback **contains** the call bag, and
the first argument names `R`. Per-arm `with_lock` in a base case does **not**
cover the parallel region (the combine still runs unlocked). Nested
`with_lock` around `rec(n-1) + rec(n-2)` **does** cover.

First cut does **not** treat a held mutex as making the body pure, and does
**not** emit `__coil_par_*` workers for covered bags. That keeps F0–F2
behavior intact for pure boards and avoids I/O reordering / deadlock from
compiler-inserted forks.

Compile-time coverage is structural (AST). It is not a proof that every
dynamic path holds `R`, and it is not deadlock-free.

## Runtime check story (not shipped)

When admit is allowed later, all of these should hold:

1. **Compiler:** covering lock of every named escape `R` on the bag
   (this note’s check). Missing → sequential, same as today.
2. **Spawn gate:** `thread_spawn_shared` only if each `R` is on the C0
   whitelist (Mutex / RwLock Arc already is; FD / FFI stay refuse until a
   user lock object wraps them).
3. **Runtime assert (Layer A):** at steal start, `R` is held by the
   joining mutator for the epoch. If not held → sequential leftover / isolate
   fallback, not a data race. Do not “best effort” share.
4. Stolen bodies must not **re-lock** `R` (would deadlock on a
   non-recursive mutex). That is a later lint.

User classes: same protocol. Hint names the object; lock in userland →
eligible for a later admit. Unlocked class fields stay C0 refuse.

## What F3 ships

| Piece | Status |
|---|---|
| Internals note (this file) | this cut |
| E0805 info on refused call bags, naming `R` | this cut |
| Covering-lock detection; sequential + documented gate | this cut |
| Auto-insert locks / auto-fork on hint | **never** (model) |
| Runtime hold assert / admit steal | **not this cut** |
| Loop-IPA impure-body hints | later |
| Compiler per-FD locks | out of scope |

Prove: one smoke that a refused FD escape emits the named hint; one covering
mutex path that documents the gate and still stays sequential. A4
`COIL_AUTO_PAR=0` flagships unchanged (hints are diagnostics; IPA off).
Do not auto-merge — Architect accept for this note.
