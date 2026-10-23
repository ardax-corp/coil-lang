# Type unification rules (summary)

Moved here from the website's types reference (#1880).

Unification is structural (Robinson) with an occurs check.

| Left | Right | Result |
|------|-------|--------|
| Same `Ty::Var` | | Success |
| Same `Ty::Con` name | | Success |
| `Ty::Con(n)` | `Ty::Sum { name: n, .. }` | Isorecursive expand-and-unify |
| `Ty::Fun` | `Ty::Fun` | Unify args, then returns |
| `Ty::Tuple` | `Ty::Tuple` | Same length; unify each element |
| `Ty::Array` | `Ty::Array` | Unify elements; lengths compatible if either is `Dynamic` or both `Static` with same N |
| `Ty::Record` | `Ty::Record` | Same fields (sorted by name); unify each field type |
| `Ty::Sum` | `Ty::Sum` | Same enum name; same variant names, shapes, arities; unify payload types |
| `Ty::Constructor` | `Ty::Sum` / other constructor | Owner must unify; distinct tags of the same enum join at the parent (refinements peel when binding a `Ty::Var`) |
| `Ty::Var` | anything | Bind variable (if occurs check passes) |
| Otherwise | | `Type mismatch` error |

## Length compatibility (arrays)

```
Dynamic  ~  Static(N)   ✓
Dynamic  ~  Dynamic     ✓
Static(N) ~ Static(M)    ✓ (element types must unify; N and M need not match for unification of annotation vs literal in all cases — mismatched static lengths error when both static)
```
