// String `==` / `!=`, `+` and literal `format` calls lower from HIR. `+`
// is `FORMAT "%s%s"` with the format string under both operands, and a
// call or `?` operand is not staged through temps. The language harness
// also runs under `--hir`, so each case pins both codegens.
use string::format;

fn name() -> Result<string, string> {
    return Result::Ok("coil");
}

fn join(string a, string b) -> string {
    return a + "/" + b;
}

fn joined() -> Result<string, string> {
    let j = join("a", name()?);
    return Result::Ok(j);
}

fn show(int n, float x, string s) -> string {
    return format("n=%i x=%f s=%s %%", n, x, s);
}

test("equality and concatenation") {
    assert(join("x", "y") == "x/y")?;
    assert(join("x", "y") != "y/x")?;
    assert("<" + join(join("a", "b"), "c") + ">" == "<a/b/c>")?;
}

test("a ? operand keeps its value") {
    let got = match joined() {
        Result::Ok(s) => s,
        Result::Err(e) => e,
    };
    assert(got == "a/coil")?;
}

test("literal format") {
    assert(show(3, 1.5, "hi") == format("n=%i x=%f s=%s %%", 3, 1.5, "hi"))?;
    assert(format("%s-%i", "k", 7) == "k-7")?;
}
