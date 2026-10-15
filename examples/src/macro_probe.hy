// Macros for tests/macros/*_probe*.hy and macro_hygiene.hy: each one reports
// what the `macro` model saw, or exercises one `quote` feature.
use macro::{Expr, FnDecl, TypeDecl, Code, lit, lit_int, raw, ident, join, concat};

fn yes_no(bool b) -> string {
    if b {
        return "y";
    }
    return "n";
}

/// `inspect_T()`: what the derive saw of the type, as one string.
/// `#[probe(label = "…")]` on a field or variant is read back.
derive Inspect(TypeDecl t) -> Code attrs(probe) {
    let text = t.kind + " " + t.name.str();
    text += " class=" + yes_no(t.is_class()) + " enum=" + yes_no(t.is_enum());
    let gens = "";
    for g in t.generics {
        gens += "<" + g.str() + ">";
    }
    text += " generics=" + gens;
    text += " repr=" + t.repr;
    text += " tagged=" + yes_no(t.has_attr("tagged")) + " has_repr=" + yes_no(t.has_attr("repr"));
    text += " self=" + t.self_type().src();
    for f in t.fields() {
        text += " | " + f.name.str() + ":" + f.ty.str() + " head=" + f.ty.head() + " args=" +
                len(f.ty.args()).show();
        text += " pub=" + yes_no(f.is_pub) + " label=" + f.attr_str("probe", "label", "-");
    }
    for v in t.variants() {
        text += " | " + v.name.str() + " " + v.shape + "/" + v.arity().show();
        if v.value != "" {
            text += "=" + v.value;
        }
        text += " label=" + v.attr_str("probe", "label", "-");
    }
    let name = ident("inspect_" + t.name.str());
    return quote items {
        fn ${name}() -> string {
            return ${lit(text)};
        }
    };
}

/// A marker attribute that keeps the type unchanged.
attr tagged(TypeDecl t) -> Code {
    return raw(t.source);
}

/// Keep `f` and add `<name>_info()` (a static method on a method's class):
/// what the attribute saw of the function.
attr fn_info(FnDecl f) -> Code {
    let text = f.name.str() + "(";
    let first = true;
    for p in f.params {
        if !first {
            text += ", ";
        }
        text += p.ty.str() + " " + p.name.str();
        first = false;
    }
    text += ") -> " + f.ret.str();
    let tps = "";
    for tp in f.type_params {
        tps += tp.str();
    }
    text += " tparams=" + tps;
    text += " pub=" + yes_no(f.is_pub) + " static=" + yes_no(f.is_static) + " gen=" +
            yes_no(f.is_coro);
    text += " owner=" + f.owner;
    text += " args=" + f.arg_names();
    let info = ident(f.name.str() + "_info");
    if f.owner != "" {
        // `source` starts at `fn` / `static fn`: put `pub` back.
        let kept = f.source;
        if f.is_pub {
            kept = "pub " + kept;
        }
        return quote items {
            ${raw(kept)}
            pub static fn ${info}() -> string {
                return ${lit(text)};
            }
        };
    }
    return quote items {
        ${raw(f.source)}
        fn ${info}() -> string {
            return ${lit(text)};
        }
    };
}

/// Keep `f` and add `<name>_body()`: its body as written, trimmed of braces.
attr body_text(FnDecl f) -> Code {
    let name = ident(f.name.str() + "_body");
    return quote items {
        ${raw(f.source)}
        fn ${name}() -> string {
            return ${lit(f.body_source())};
        }
    };
}

/// Keep `f` and add `<name>_flags()`: the three arguments, one per type.
attr flags(FnDecl f, string label, int times, bool loud) -> Code {
    let text = label;
    let i = 1;
    while i < times {
        text += "," + label;
        i += 1;
    }
    if loud {
        text += "!";
    }
    let name = ident(f.name.str() + "_flags");
    return quote items {
        ${raw(f.source)}
        fn ${name}() -> string {
            return ${lit(text)};
        }
    };
}

/// `sum_into!(target, xs)`: add up `xs` with a loop whose `acc` and `x` are
/// the macro's own, then store the total in `target`.
macro sum_into(Expr target, Expr xs) -> Code {
    return quote stmts {
        let acc = 0;
        for x in ${xs} {
            acc += x;
        }
        ${target} = acc;
    };
}

/// `bump!(v)`: `v += 1` through a macro-local `tmp`.
macro bump(Expr v) -> Code {
    return quote stmts {
        let tmp = 1;
        ${v} = ${v} + tmp;
    };
}

/// `once!(e)`: `e` evaluated a single time, then doubled.
macro once(Expr e) -> Code {
    return quote expr { twice_of(${e}) };
}

fn twice_of(int n) -> int {
    return n * 2;
}

/// `texts!(a, b, …)`: every argument's source joined with `|`.
macro texts(Vec<Expr> xs) -> Code {
    let parts: Vec<Code> = Vec::new();
    for x in xs {
        parts.push(raw(x.str()));
    }
    return lit(join(parts, "|"));
}

/// `glued!(a, b, …)`: the arguments' sources joined by `concat` (one per
/// line), as one literal.
macro glued(Vec<Expr> xs) -> Code {
    let parts: Vec<Code> = Vec::new();
    for x in xs {
        parts.push(raw(x.str()));
    }
    return lit(concat(parts).src());
}

/// `kinds!(a, b, …)`: each argument as `kind/literal?/ident?`, space separated.
macro kinds(Vec<Expr> xs) -> Code {
    let out = "";
    for x in xs {
        if out != "" {
            out += " ";
        }
        out += x.kind() + "/" + yes_no(x.is_literal()) + yes_no(x.is_ident());
    }
    return lit(out);
}

/// `count_args!(…)`: how many arguments the call had.
macro count_args(Vec<Expr> xs) -> Code {
    return lit_int(len(xs));
}

/// `list_of!(Name, T)`: `type Name = Vec<T>;`, built with `quote type`.
macro list_of(Expr name, Expr elem) -> Code {
    let ty = quote type { Vec<${elem}> };
    return quote items {
        type ${name} = ${ty};
    };
}

/// `pick!(c, a, b)`: `a` when `c`, else `b`, as one expression.
macro pick(Expr c, Expr a, Expr b) -> Code {
    return quote expr { choose(${c}, ${a}, ${b}) };
}

fn choose(bool c, int a, int b) -> int {
    if c {
        return a;
    }
    return b;
}

/// `getter!(Name, field)`: a class `Name` with one `int` field and a getter.
macro getter(Expr name, Expr field) -> Code {
    let get = ident("get_" + field.str());
    return quote items {
        class ${name} {
            pub ${field}: int,
        }

        impl ${name} {
            pub fn ${get}() -> int {
                return self.${field};
            }
        }
    };
}
