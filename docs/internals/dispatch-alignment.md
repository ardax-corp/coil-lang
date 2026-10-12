# VM dispatch alignment

The VM's dispatch loop ends in one indirect jump (`jmp *%rcx`) that every
opcode goes through. On some CPUs its speed depends on where that jump lands
inside its 64-byte cache line, and without help that position is decided by
whatever code the linker happened to place before the loop.

## What happened (2026-10-12)

A packaged program ran fib about 40% slower than `coil run` on the same
archive on a Ryzen 7 7700 (Zen 4). Both binaries run the same VM code
(`coil_cli::execute_archived_program`) and executed the same instructions:

| fib, sequential | `coil run` | packaged (`coil-embed`) |
|---|---|---|
| instructions | 660M | 659M |
| cycles | 136M | 195M |
| front-end stall cycles | 9M | 55M |
| wall time | 27.3 ms | 38.7 ms |

The only difference was the dispatch jump's offset in its line: 0x1b in
`coil`, 0x3b in `coil-embed`. `coil-embed` is a much smaller binary, so fat
LTO lays its code out differently. A fresh build of the same source in
another directory flipped it (`coil run` slow at 0x2b, packaged fast at 0x1b).
On a 4-core cloud x86 box the two were at parity: the effect is CPU-specific.

## Fix

`.cargo/config.toml` passes `-C llvm-args=-align-loops=64` on x86_64. Loop
headers start on a 64-byte boundary, so the jump's offset no longer depends on
the surrounding code: 0x1b in both binaries in every build measured.

Zen 4, sequential, `coil run` / packaged (ms):

| bench | default | `-align-loops=64` |
|---|---|---|
| fib | 27.1 / 34.7 | 28.4 / 28.0 |
| nbody | 64.4 / 70.7 | 67.6 / 66.6 |
| int_match | 102.5 / 105.3 | 104.3 / 105.1 |

Size: `coil` +0.7 MB, `coil-embed` +0.1 MB.

This makes the position deterministic, not optimal everywhere: which offset is
fast is microarchitecture-specific. The layout-proof fix is threaded dispatch
(each handler jumping to the next on its own), which removes the single hot
jump. The fn-pointer table tried in [threaded-dispatch.md](threaded-dispatch.md)
still went through one trampoline jump, and its A/B numbers predate this
finding, so they may carry the same layout noise.

## Tried and rejected

- `-align-all-functions=6`: moved the jump to 0x3b in both binaries (both slow).
- PGO + BOLT (trained on examples, language tests and library suites, perf
  benches held out): consistent but slow (0x32 in both, fib ~35 ms), small wins
  elsewhere (for_in_iter -6%, gc_churn -4%), 2-3x binary size, a ~20 minute
  build, and BOLT cannot rewrite libffi's trampoline with `-use-old-text`.
- Raising LLVM tail-duplication limits: still one shared dispatch jump.

## Checking a build

```
objdump -d --no-show-raw-insn target/release/coil-embed \
  | grep -A5 -P 'cmovae\s+%ecx,%edx' | grep -P 'jmp\s+\*%rcx'
```

Take the address modulo 64.
