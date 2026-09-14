# Perf free-tickets artifacts

Companion dumps for [`docs/internals/perf-free-tickets.md`](../../docs/internals/perf-free-tickets.md).

- `dissect/` — `coil dissect` text + opcode histograms
- `opt-stats/` — `--opt-stats-json` stderr
- `times/` — hyperfine markdown
- `flames/` — `cpu-clock` flamegraph SVGs + small `.hyc` folded stacks
- `parse_dissect.py` / `collect.sh` — regenerate locally (`COIL_AUTO_PAR=0`, `--root` stdlib)

Do not commit `.hyc` or packaged binaries (regenerate with `collect.sh`).
