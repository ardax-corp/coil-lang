// Function-style macros used by `tests/positive/function_macro.hy` and
// `examples/macro_fn.hy`.
use macro::{Expr, Code, lit, raw};

/// `square!(e)`: `e * e`, with `e` kept whole (`square!(1 + 2)` is 9).
macro square(Expr e) -> Code {
    return quote expr { ${e} * ${e} };
}

/// `check!(cond)`: panics with the condition as written when it is false.
macro check(Expr cond) -> Code {
    return quote stmts {
        if !${cond} {
            panic "check failed: " + ${lit(cond.str())};
        }
    };
}

/// `sum!(a, b, …)`: the sum of every argument (`0` for none).
macro sum(Vec<Expr> xs) -> Code {
    if len(xs) == 0 {
        return raw("0");
    }
    let parts: Vec<Code> = Vec::new();
    for x in xs {
        parts.push(raw(x.src()));
    }
    return quote expr { $(parts)+* };
}

/// `quad!(e)`: `square!(square!(e))`, expanded in the next round.
macro quad(Expr e) -> Code {
    return quote expr { square!(square!(${e})) };
}

/// `describe!(e)`: the argument's kind and text, as a string.
macro describe(Expr e) -> Code {
    return lit(e.kind() + ": " + e.str());
}

/// `counter!(Name)`: a class `Name` with a `bump` method, at the top level.
macro counter(Expr name) -> Code {
    return quote items {
        class ${name} {
            pub n: int,
        }

        impl ${name} {
            pub fn bump() -> int {
                self.n += 1;
                return self.n;
            }
        }
    };
}

/// `swap!(a, b)`: exchange two local variables.
macro swap(Expr a, Expr b) -> Code {
    return quote stmts {
        let tmp = ${a};
        ${a} = ${b};
        ${b} = tmp;
    };
}
