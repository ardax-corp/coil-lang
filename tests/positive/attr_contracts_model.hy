// `FnDecl` carries `requires` / `ensures` as written, and `with_name`
// keeps them.
use derive_macros::contracted;

#[contracted]
fn half(int n) -> int
    requires n >= 0, "negative input"
    ensures result * 2 <= n
{
    return n / 2;
}

#[contracted]
fn plain() -> int {
    return 7;
}

test("the macro model sees contract clauses") {
    assert(half_contracts() == " requires n >= 0, \"negative input\" ensures result * 2 <= n")?;
    assert(plain_contracts() == "")?;
}

test("with_name keeps the contracts and the body") {
    assert(half_kept(9) == 4)?;
    assert(plain_kept() == 7)?;
}
