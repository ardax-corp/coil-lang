# SIMD lanes and shapes (`feature/simd-lanes`)

Plan for the next round of loop vectorisation after `byte_scan` on
`Vec<byte>` (#2075), `bulk_push` (#2077), `VLoop` anywhere (#2078) and the
min / max / count / bit folds (#2080). The parser cursors in coil-json,
coil-csv and coil-toml were moved to local-index scans first (json#19,
csv#7, toml#14), so their runs now reach `VScan` and `vec_extend_range`.

One PR per step into `feature/simd-lanes`; local verification gates the
step PRs and full CI runs on the final squash-merged PR into `main`.
No new opcodes: every step adds `VBin` kinds, a `VLoop` body word or a
HostInvoke id, and bumps the archive minor when an old runtime would
reject it.

## Where it stands (main `a0f9a69`, release, `COIL_AUTO_PAR=0`)

Probe: `simd-expansion/probes/p11_new_shapes.hy` (project files).

| Shape | Example | Now | Taken by |
|---|---|---|---|
| two-stream compare, `break` form | deflate match length | 64 µs / 512 KiB | `VScan` |
| two-stream compare, flag form | `bytes::region_eq` | 31.6 ms / 512 KiB | nothing |
| any-of scan on a string | `text::is_ascii` | 60 µs / MiB | `VScan` |
| backward scan | `rfind` on `Vec<byte>` | 123 µs / MiB | `VScan` backward |
| map-push with a select | `text::shift_ascii`, `ascii::to_lower` | 62 ms / MiB | nothing |
| map-push | `out.push(a[i] * 3 + 1)` | 27 ns / elem | nothing |
| store map with a select | abs, clamp | 21 ns / elem | nothing |
| store map with lane casts | `(a[i] as int) as float` | 19 ns / elem | nothing |
| copy from a string | `out.push(s[i])` | 27 ms / MiB | nothing |
| copy until a byte | JSON string body, one loop | 44 ms / MiB | nothing |
| userland math maps | `floor`, `fabs`, `sqrt` | 95 / 18 / 880 ns / elem | nothing |

For scale, a `VLoop` store map is about 4 ns per element and a
`vec_extend_range` copy about 0.07 ns per byte.

## Steps

1. **Lane casts.** `VBin` kinds for int-to-float and float-to-int
   (truncating, as `as int` does, trapping where it traps) on any lane,
   not only a splat. `VOp::CastI2F` takes any source; a new `CastF2I`.
2. **Selects in map bodies.** If-convert a diamond or triangle whose arms
   are pure and cannot trap (the `match_cond_reduce_on` rule) into a
   `VOp::Select { mask, then, else }`: a compare lane picks per lane. One
   new body word (`VSel`, three registers like `VFma`). Covers abs,
   clamp, `if x < 0 { x = -x }`, and once inlined, `math::fabs`,
   `floor`, `ceil`.
3. **Byte maps.** A `Vec<byte>` already runs as int lanes (`as byte`
   and `as int` are `& 255`), but `(c as int) + 32` in an arm is an
   `int` add that could trap. An arm may now add, subtract and multiply
   when the operands' ranges (constants, non-negative masks) keep the
   result in range, so `out[i] = to_lower(b[i])` runs 8 lanes a time.
4. **Map-push.** A counted loop whose body is one `out.push(value(i))`,
   or a push in each arm of an `if`, runs as a `VLoop` whose body ends in
   an append (`VStore` flag `STORE_PUSH`): each block appends its 8 lanes
   once its other words ran. A trap replays the block lane by lane, so it
   leaves the pushes before it, as the scalar loop would. The guard
   checks `out` is not an array the body reads or the bound's. Covers
   `shift_ascii` and every `out.push(f(a[i]))` with a body steps 1–3
   take.
5. **Copy from a string.** `vec_extend_range` takes a `string` source,
   so `out.push(s[i])` runs as one copy. An older runtime answers the
   start for a source it does not copy, so the loop runs scalar there.
6. **Copy until a byte.** A loop that scans for a stop set and pushes
   every other byte becomes `VScan` to the stop, then one
   `vec_extend_range` of the run, with `i` and `out` as the loop leaves
   them.
7. **Library follow-ups** (their own repos, after the branch lands):
   `bytes::region_eq` in the `break` form so `VScan` takes it;
   `text::shift_ascii` checked under step 4; coil-json / coil-csv /
   coil-toml copy loops collapsed where step 6 makes the split form
   unnecessary.

Each step proves itself on `examples/perf` hit benches (the flagships
do not have these loops), per the hit-bench rule in
`optimization-roadmap.md`.

## Progress

| Step | PR | Hit bench (`vec_lanes.hy`, 8192 elements) |
|---|---|---|
| 1 lane casts | #2137 | trunc map 172.8 → 38.5 µs, int-to-float map 154.5 → 49.7 µs |
| 2 selects | #2140 | abs 170 → 76 µs, clamp 178 → 72 µs |
| 3 byte maps | #2142 | ASCII lower case on `Vec<byte>` 279 → 101 µs |
| 4 map-push | #2143 | `out.push(a[i] * 3 + 1)` 284 → 69 µs, `shift_ascii` shape on `Vec<byte>` 504 → 127 µs |
| 5 copy from a string | this branch | `out.push(s[i])` over 1 MiB: 32 ms → about 0.15 ms (`vec_bulk.hy`) |

Step 3 also fixed `VLoop` lane loads from a packed `Vec<byte>`, which
skipped to the block through the element iterator, so the loads were
quadratic in the array's length. The body words now reuse a register
once its last reader ran, so a 12-node body fits the 8 registers.

Seen along the way: a `VLoop` body word costs about 9 ns per block of
8 lanes (the interpreter loop in `machine::vm::vloop` copies registers
per word), so a 6-word body runs at about 9 ns per element. Tuning that
loop helps every step here and is a candidate after step 6.

## Not in this plan

- **`sqrt` lanes.** The userland `math::sqrt` is a loop with an integer
  rounding fix-up, so no select or cast makes it a lane body. A lane
  sqrt needs a float primitive the compiler can see (an operator or a
  prelude intrinsic), which is a language decision.
- **`sin` / `exp` lanes.** Range reduction branches on the input; they
  could follow step 2 once the reductions are written select-shaped, and
  are measured again after it.
- **Gathers and scatters** (`counts[b[i]] += 1`, table crc32): no lanes.
