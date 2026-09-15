# Auto-par vs branch misses (investigation)

Report-only investigation on main `f35b3ea4` G3 (no ISA / codegen change), plus
**COI-390** join wait (drop 1 ms poll) and **COI-391** idle park (drop 2 ms
empty-steal). Symptom: `poop` shows many **branch misses** when `COIL_AUTO_PAR`
is on.

## How to read `poop` here

`poop` / `perf stat` hardware events need a CPU PMU
(`/sys/bus/event_source/devices/cpu` or `cpu_core`). Cloud Agent kernels in this
repo often expose only `software` / `breakpoint` / `msr`. Then:

- `poop` panics (`perf_event_open` ENOENT, stripped binary).
- `PERF_COUNT_HW_BRANCH_*` is unavailable.
- Software events still work: task-clock, context-switches, page-faults,
  migrations.

On a laptop with a PMU, compare **miss rate** (`branch-misses / branches`) and
per-thread counts, not raw process-wide misses. Auto-par starts extra OS
threads; `inherit` counters **sum** every worker.

Optional reactor dump (off by default):

```bash
COIL_PAR_STATS=1 COIL_MAX_WORKER_THREADS=4 ./target/release/coil run fib.par.hyc
# coil par-stats workers=… submitted=… steal_ok=… steal_empty=… steal_retry=…
# idle_waits=… join_helps=… join_timeouts=… join_parks=…
```

Fair sequential `.hyc`: `COIL_AUTO_PAR=0` at **compile**. Runtime
`COIL_AUTO_PAR` does not rewrite an archive. `scripts/poop_baseline.sh` already
compiles `fib` with `COIL_AUTO_PAR=0`; mandelbrot/nsieve/tak/binary_trees do not
emit IPA either (identical bytecode with the flag on).

## Reproduce (this host: 4 vCPU Xeon, fat LTO release, no HW PMU)

Compile with `--root .deps/coil-stdlib/src`. Checksums unchanged:
mandelbrot `625885`, fib `2178309`, nsieve `1900`, tak `7`,
loop_ipa_sum `2666646666700000`.

### Bytecode: who actually parallelizes

| Bench | `COIL_AUTO_PAR=0` vs default | IPA? |
|---|---|---|
| `mandelbrot` | identical 2876 B | no (nested mut locals, not a counted reduce) |
| `nsieve` | identical 2656 B | no (heap stores) |
| `binary_trees` | identical 3492 B | no (grain / sendability) |
| `tak` | identical 2236 B | no (narrow grain) |
| `fib` | 2204 vs 3112 B, `__coil_par_fib` | yes (F1 hop, 3 `thread_spawn_shared`) |
| `loop_ipa_sum` | 2152 vs 2776 B, `__coil_par_loop_1` | yes (one chunk job) |

### Wall (`hyperfine`, bash, ≥30 runs)

| Config | mean wall | notes |
|---|---|---|
| mandelbrot seq / par archives | 18.6 ± 1.4 / 18.5 ± 0.7 ms | wash (same `.hyc`) |
| mandelbrot `COIL_THREADED_DISPATCH=0` | 17.6 ± 0.2 ms | giant match ~1.06× vs default table |
| nsieve seq / par | 2.3 / 2.3 ms | wash |
| fib sequential archive | **56.9 ± 0.5 ms** | CALL kernel, reactor never starts |
| fib IPA, 4 workers | **24.6 ± 0.9 ms** | user+sys ~61 ms (parallel CPU) |
| fib IPA, 1 worker | 38.6 ± 1.5 ms | still forks; help-steal |
| fib IPA, 4 workers, match dispatch | 24.1 ± 0.6 ms | wash vs table |
| loop_ipa_sum seq | 8.3 ± 0.4 ms | |
| loop_ipa_sum IPA, 4 workers | 3.7 ± 0.4 ms | |
| loop IPA + match dispatch | 3.0 ± 0.3 ms | short; not a G3 regression claim |

### Software counters (`perf_event` inherit, per run ≈ total/reps)

| Config | wall/run | task-clock/run | ctx switches/run | page faults/run |
|---|---|---|---|---|
| fib seq | 57.9 ms | 57.5 ms | **1.3** | 212 |
| fib IPA w=4 | 25.8 ms | 61.3 ms | **61** | 353 |
| fib IPA w=1 | 38.2 ms | 59.4 ms | 17 | 313 |
| fib IPA w=4 match | 25.1 ms | 58.7 ms | 53 | 351 |
| mandelbrot seq vs par | 18.8 / 18.7 ms | 18.5 / 18.4 ms | 1.7 / 1.6 | ~213 |
| loop seq vs IPA w=4 (20 reps) | 8.4 / 4.0 ms | 8.1 / 6.1 ms | 1.3 / **18** | 210 / 305 |

Process-wide **context switches** jump with auto-par (~50× on fib, ~14× on
loop IPA). That is the portable stand-in for “lots of extra control-flow /
wakeups” when branch-miss counters are missing. Sequential flagships do not
move.

### `COIL_PAR_STATS=1` (one run)

| Config | workers | submitted | steal_ok | steal_empty | steal_retry | idle_waits | join_helps | join_timeouts |
|---|---|---|---|---|---|---|---|---|
| fib seq / mandelbrot / nsieve | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| fib IPA w=4 | 4 | **3** | 3 | **~300** | **0** | ~23 | 0 | ~27 |
| fib IPA w=1 | 1 | 3 | 2 | ~30 | 0 | 1 | 2 | ~14 |
| fib IPA + `COIL_SHARED_HEAP=0` | 4 | 3 | 3 | ~342 | 0 | ~29 | 0 | ~30 |
| loop IPA w=4 | 4 | **1** | 1 | ~41 | 0 | 8 | 0 | 1 |

F1 hop means fib(32) is **three** AlwaysPar spawns, not a job per tree node.
Empty steals dominate successful steals (~100:1 at w=4) **before** COI-391.
`Steal::Retry` (CAS contention on the deque) is **zero** on this load. Before
COI-391, idle workers `wait_timeout(2 ms)` and (before COI-390) the joiner
`wait_timeout(1 ms)` then steal-empty again.

### COI-390 join wait (after)

Join parks on `sleep_cvar` until a result or stealable job (`notify` holds the
sleep mutex and `notify_all`). No 1 ms poll. Idle 2 ms park is unchanged.

| Config | steal_empty | idle_waits | join_timeouts | join_parks | notes |
|---|---|---|---|---|---|
| fib IPA w=4 **before** | ~300 | ~23 | **~27** | — | 1 ms join poll |
| fib IPA w=4 **after** | ~200 | ~30 | **0** | **4** | 3 jobs; parks not timeouts |
| loop IPA w=4 **before** | ~41 | 8 | 1 | — | |
| loop IPA w=4 **after** | 33 | 7 | **0** | 0 | result ready without park |

Fib IPA wall on this host (release, `hyperfine`): seq **66.9 ± 4.7 ms**, IPA
w=4 **28.1 ± 15.5 ms** (min 24.2 ms; noisy outliers). Checksums hold:
mandelbrot `625885`, fib `2178309`, nsieve `1900`. Sequential mandelbrot /
nsieve archives remain byte-identical with auto-par on.

### COI-391 idle park (after)

Idle workers recheck steal/shutdown under `sleep` then `wait` until `notify`
(submit / job-complete / shutdown). No 2 ms poll. When `inflight == 0` they
skip the empty-deque walk. Join path unchanged (`join_timeouts=0`).

| Config | steal_empty | idle_waits | join_timeouts | join_parks | notes |
|---|---|---|---|---|---|
| fib IPA w=4 **COI-390** | ~200 | ~30 | **0** | **4** | 2 ms idle poll |
| fib IPA w=4 **COI-391** | **~140** | **~12** | **0** | **4–5** | parks, not 2 ms timeouts |
| loop IPA w=4 **COI-390** | 33 | 7 | 0 | 0 | |
| loop IPA w=4 **COI-391** | **~20–36** | **6–7** | **0** | 0–1 | |

Leftover `steal_empty` is `notify_all` waking idle peers who then walk injector
+ stealers once (counted per deque). It no longer grows with wall / 2 ms.

Fib IPA wall on this host (release, `hyperfine` ≥30 runs): seq **57.2 ± 0.4 ms**,
IPA w=4 **25.2 ± 1.3 ms** (min 23.9 ms). Sequential mandelbrot **18.5 ± 0.4 ms**,
nsieve **2.6 ± 0.2 ms**. Checksums hold: mandelbrot `625885`, fib `2178309`,
nsieve `1900`, loop_ipa_sum `2666646666700000`. Sequential mandelbrot / nsieve
archives remain byte-identical with auto-par on.

## Attribution

### 1. Reactor idle / join poll (primary under auto-par)

`machine/src/reactor.rs`: **Join** (COI-390) waits until result or job —
`join_timeouts` is 0. **Idle** (COI-391) parks until `notify` — no 2 ms poll.

G0–G3 execute peeks are **not** on this path.

### 2. Execute dispatch / ALWAYS_HOT peeks (G3, X2/S3/S5/X4, DenseBin2)

Default table: `unlikely(is_hot)` divert into `execute_dense` (compact
`ALWAYS_HOT` match, no 256-entry table), then leftover `table_loop`.
Table/hotmatch peek trailing `JMP` (X2), `DenseCast`+bin (S3), store+IV+JMP
(S5), `DenseBin2` residue (X4). Giant match does not peek.

- **Fib IPA workers** stay on CALL/RETURN (kernel ops). They **do not** enter
  `execute_dense`. Table vs match wall on fib IPA is a wash (24.6 vs 24.1 ms).
- **Mandelbrot** is sequential dense: G3 peeks apply, auto-par does not start
  the reactor. Match is slightly faster here (17.6 vs 18.6 ms), same as
  [threaded-dispatch.md](threaded-dispatch.md) G3 notes.
- **loop_ipa_sum** workers run a dense int loop **and** one steal. Extra
  ctx-switches still track the idle pool, not peek density.

Hypothesis “G3 ALWAYS_HOT peek raises mispredict **rate** per worker when the
bytecode mix differs”: **not supported** for fib IPA (no dense streak). For
mandelbrot, peeks exist without auto-par. A host with PMU should sample
`execute_dense` vs `steal_from_injector` IPs to confirm rates.

### 3. Shared-heap / IPA vs sequential

C1/C2 `thread_spawn_shared` + Layer A epoch (`AtomicUsize` jobs, `Mutex`
alloc lock, abort flag). Fib arms are immediates; `COIL_SHARED_HEAP=0` did not
change steal_retry (still 0) or wall in the software-counter noise. Contention
as CAS-spin **branch misses** is **not** visible on these benches.

Chunked for (`loop_ipa_sum`) is one split, not a steal storm.

### 4. F3 lock hints

Compile-time E0805 only. No runtime lock in flagship execute. Not a miss
source.

## Ranked next steps (Coil-shaped)

| Rank | Change | Cost | Why |
|---|---|---|---|
| 1 | ~~Join wait without 1 ms poll~~ **landed (COI-390)** — `join_timeouts=0`, `join_parks≈4` on fib IPA | small reactor | Remaining empty-steals were idle 2 ms |
| 2 | ~~Idle workers park until `notify`~~ **landed (COI-391)** — `idle_waits≈12`, `steal_empty≈140` on fib IPA (was ~30 / ~200) | small reactor | Cuts timed empty-steal; leftover empties are one walk per notify |
| 3 | **Read `poop` as miss rate + pin `COIL_MAX_WORKER_THREADS`** when comparing seq vs par | docs / script | Absolute misses scale with threads; `poop_baseline.sh` fib is already sequential |
| 4 | **Do not revert G3 peeks / DenseBin2 for this symptom** | — | Fib IPA never takes them; mandelbrot auto-par is a no-op |
| 5 | Optional: start `n = min(cap, inflight)` workers, or keep a 1-worker pool until the second submit | medium | Avoids 3 idle pollers for a 3-job fib |
| 6 | PMU laptop: `perf record -e branch-misses` on fib IPA vs seq; expect hits in `steal_from_*` / pthread condwait, not `exec_dense` | measurement | Confirms (1) if this cloud’s missing PMU worried anyone |
| 7 | Shared-heap CAS / F3 | skip for this | No evidence on flagships |

Rank 1–2 landed (COI-390, COI-391). Free leftover: (3). Large design: changing hop/grain so fib spawns more
jobs (would **increase** steal traffic). ISA peeks are the wrong knob.

## Hypotheses (verdict)

| Hypothesis | Verdict |
|---|---|
| Auto-par steal / chunk / reactor poll dominate misses more than opcode peeks | **Hold** (idle + join polls gone; leftover is real steal + condwait; 3 jobs; retry=0) |
| G3 `execute_dense` ALWAYS_HOT peek + table raises per-worker mispredict when mix differs | **Discard for fib IPA**; sequential mandelbrot only |
| Shared-heap contention shows as misses around CAS/spin | **Discard on these benches** (`steal_retry=0`, shared-off wash) |
