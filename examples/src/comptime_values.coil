// Macros that read their arguments' values, for `tests/comptime/`.
use macro::{Code, Expr, lit, lit_int, raw};

/// `repeat!(text, n)`: `text` repeated `n` times, as a string literal.
macro repeat(Expr text, Expr n) -> Code {
    let s = text.string() ?? "";
    let out = "";
    let i = 0;
    while i < (n.int() ?? 0) {
        out += s;
        i += 1;
    }
    return lit(out);
}

/// `describe_value!(e)`: what `e`'s value is, as a string literal.
macro describe_value(Expr e) -> Code {
    if let Option::Some(n) = e.int() {
        return lit("int " + n.show());
    }
    if let Option::Some(b) = e.bool() {
        return lit("bool " + b.show());
    }
    if let Option::Some(s) = e.string() {
        return lit("string " + s);
    }
    if let Option::Some(f) = e.float() {
        return lit("float " + f.show());
    }
    return lit("not a constant");
}

/// `half!(x)`: `x / 2` folded while compiling, for a float constant.
macro half(Expr x) -> Code {
    return raw(((x.float() ?? 0.0) / 2.0).show());
}
