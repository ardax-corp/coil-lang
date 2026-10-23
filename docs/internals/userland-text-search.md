# Userland text search

`split`, `find_all` and `replace_all` on strings are written in Coil, in
coil-stdlib's `text` module (`Searchable` default methods over a
`Pattern`). The goal (2026-10-22) is to keep them there and make the
compiler good enough that they run close to host natives. They should not
move into the VM. No ISA-specific SIMD is used.

The work happens on the feature branch `feature/userland-loop`, one PR per
step.

## Where the time went

Measured on main at `d1d741d`: a CSV row of about 60 bytes, split at `,`
20 000 times. That is about 1.30G instructions under callgrind, or about
8 000 instructions per match. Byte scanning was about 4% of it. The rest
was per-match overhead:

| Cost | Share | Cause |
|------|-------|-------|
| Dispatch in the shared body | ~30% | Trait defaults with their own bound (`fn split<P: Pattern>`) were never cloned, so every call ran the generic body with a dictionary `CallIndirect` per `find_in` |
| `find_object_by_addr` | ~19% | Slab membership check on every string argument to a host native |
| GC / allocation | ~12% | A `Span` object per match (inside `Option`, plus the `?? new Span(-1, -1)` default) and the substring slices |

## Steps

| Step | PR | Change | CSV-row split instructions |
|------|----|--------|----------------------------|
| — | main `d1d741d` | baseline | 1.299G |
| 1 | (reverted) | Let loops without arithmetic into dense | 1.304G (no gain) |
| 2 | #1925 | Per-type clones of trait defaults at their own bounds' types: `Searchable__default__split$inst$string_string` is dense and inlines `find_in` | 1.017G (−22%) |
| 3 | this step | MIR `phi_sroa`: thread the niche `None` test through its φ, then split a φ of fresh objects read only field by field into per-field φs. The `Span` per match goes. | 0.772G (−24%, −41% total) |

Wall clock (`coil` release, ns per call, median of 3, x86-64 container):

| Input | main `d1d741d` | after step 3 | host natives (#1900, parked) |
|-------|----------------|--------------|------------------------------|
| csv-row split | 8 000 | 4 900 | 1 600 |
| 1 KB text split | 39 000 | 25 000 | 7 100 |
| log-line split | 1 580 | 860 | — |
| csv-row replace_all | 11 200 | 10 600 | 1 120 |

`replace_all` still runs its shared body. It is the next target, together
with step 4: trusted typed string arguments for host natives, which skips
the slab check.

## Notes

- A `readonly class Span`, which uses the two-slot pair layout, made split
  slower (1.168G). The `??` join rebuilt the object, and `Option<Span>`
  still boxed its payload. The MIR pass covers the mutable class
  instead, as long as the object neither escapes nor is written after
  the join.
- `phi_sroa` runs last among the dense passes, after LICM and CSE have
  shaped the join. It does not run in a debugger build (`debug_keep`).
