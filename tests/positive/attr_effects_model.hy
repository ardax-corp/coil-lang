// `FnDecl` carries `pure fn` and `uses {…}`, and `with_name` keeps them.
use derive_macros::declared;

#[declared]
pure fn add(int a, int b) -> int {
    return a + b;
}

#[declared]
fn shout(string s) -> string uses {read, mutate} {
    return s + "!";
}

#[declared]
fn plain() -> int {
    return 7;
}

test("the macro model sees declared effects") {
    assert(add_effects() == "pure")?;
    assert(shout_effects() == "uses {read, mutate}")?;
    assert(plain_effects() == "none")?;
}

test("with_name keeps the declaration and the body") {
    assert(add_kept(2, 3) == 5)?;
    assert(shout_kept("hi") == "hi!")?;
    assert(plain_kept() == 7)?;
}
