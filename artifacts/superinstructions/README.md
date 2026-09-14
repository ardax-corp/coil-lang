# Superinstruction investigation dumps

Generated with release `coil` / `coil-dissect`, `COIL_AUTO_PAR=0`,
`--root .deps/coil-stdlib/src`. See
`docs/internals/superinstructions-candidates.md`.

Checksums (`coil run` of compiled `.hyc`):

| Bench | Checksum |
|-------|----------|
| mandelbrot | 625885 |
| fib | 2178309 |
| tak | 7 |
| nsieve | 1900 |
| binary_trees | 135854 |
| for_in_sum | 12884115456 |
| vec_scan | 536739840 |

`dissect/*.fn.txt` — hot function bytecode.
`dissect/*.bytecode.txt` — full program bytecode (includes stdlib helpers).
`opt-stats/*.txt` — `--opt-stats` (IL passes only).
`ngrams.txt` — weighted opcode n-grams (order-of-magnitude trip weights).
