// Built-in derives: `Show`, `Eq`, `Ord`, `Hash`, `String`, `Default`, `Send`
// and `Sensitive`. Always in scope (module `derive`); the compiler
// runs them like any user derive.
//
// Generated code matches what the former Rust expansion built: payloads are
// read by field access (`p.w`) for records and bound by the pattern for tuple
// variants; scalar-backed enums compare / show their backing value.
use macro::{TypeDecl, Field, Variant, TypeRef, Code, raw, lit};

fn format_int(int n) -> string {
    return string::format("%i", n);
}

// `a.f` for every field / record payload field, or `p0`… for tuple payloads.
fn payload_names(Variant v) -> Vec<string> {
    let out: Vec<string> = Vec::new();
    if v.is_tuple() {
        let i = 0;
        while i < v.arity() {
            out.push("p" + format_int(i));
            i += 1;
        }
    } else if v.is_record() {
        for f in v.fields {
            out.push(f.name.str());
        }
    }
    return out;
}

// `Name::V`, `Name::V(_, _)` / `Name::V(pfx0, pfx1)`, `Name::V { a: _, b: _ }`.
fn pattern(TypeDecl t, Variant v, string bind) -> string {
    let head = t.name.str() + "::" + v.name.str();
    let names = payload_names(v);
    if v.is_tuple() {
        let parts = "";
        let i = 0;
        while i < len(names) {
            if i > 0 {
                parts += ", ";
            }
            if bind == "" {
                parts += "_";
            } else {
                parts += bind + names[i];
            }
            i += 1;
        }
        return head + "(" + parts + ")";
    }
    if v.is_record() {
        let parts = "";
        let i = 0;
        while i < len(names) {
            if i > 0 {
                parts += ", ";
            }
            parts += names[i] + ": _";
            i += 1;
        }
        return head + " { " + parts + " }";
    }
    return head;
}

// The payload value `i` of `v` as seen from receiver `recv` (tuple values are
// bound by the pattern with prefix `bind`).
fn payload_value(Variant v, string recv, string bind, string name) -> string {
    if v.is_tuple() {
        return bind + name;
    }
    return recv + "." + name;
}

// `%v` format of a value: `Name { a: %v, b: %v }` / `E::V(%v, %v)` / `E::V`.
fn show_format(TypeDecl t, string p) -> string {
    if t.is_class() {
        let specs = "";
        let args = "";
        let i = 0;
        for f in t.fields() {
            if i > 0 {
                specs += ", ";
            }
            specs += f.name.str() + ": %v";
            args += ", " + p + "." + f.name.str();
            i += 1;
        }
        return "return string::format(" + lit(t.name.str() + " { " + specs + " }").src() + args + ");";
    }
    let arms = "";
    for v in t.variants() {
        let names = payload_names(v);
        let head = t.name.str() + "::" + v.name.str();
        let specs = "";
        let args = "";
        let i = 0;
        while i < len(names) {
            if i > 0 {
                specs += ", ";
            }
            if v.is_record() {
                specs += names[i] + ": %v";
            } else {
                specs += "%v";
            }
            args += ", " + payload_value(v, p, "s_", names[i]);
            i += 1;
        }
        let text = head;
        if v.is_tuple() {
            text = head + "(" + specs + ")";
        } else if v.is_record() {
            text = head + " { " + specs + " }";
        }
        arms += pattern(t, v, "s_") + " => string::format(" + lit(text).src() + args + "),\n";
    }
    return "return match " + p + " {\n" + arms + "};";
}

/// `impl Show`: `Name { a: 1, b: x }` / `E::V(1)`; scalar enums show their value.
derive Show(TypeDecl t) -> Code {
    let p = "__show_" + t.name.str();
    if t.repr != "" {
        let n = "__show_n_" + t.name.str();
        return quote items {
            impl Show for ${t.impl_head("Show")} {
                fn show(${t.self_type()} ${raw(p)}) -> string {
                    let ${raw(n)}: ${raw(t.repr)} = ${raw(p)};
                    return ${raw(n)}.show();
                }
            }
        };
    }
    return quote items {
        impl Show for ${t.impl_head("Show")} {
            fn show(${t.self_type()} ${raw(p)}) -> string {
                ${raw(show_format(t, p))}
            }
        }
    };
}

/// `impl String`: same text as `Show`.
derive String(TypeDecl t) -> Code {
    let p = "__str_" + t.name.str();
    if t.repr != "" {
        let n = "__str_n_" + t.name.str();
        return quote items {
            impl String for ${t.impl_head("Show")} {
                fn to_string(${t.self_type()} ${raw(p)}) -> string {
                    let ${raw(n)}: ${raw(t.repr)} = ${raw(p)};
                    return ${raw(n)}.show();
                }
            }
        };
    }
    return quote items {
        impl String for ${t.impl_head("Show")} {
            fn to_string(${t.self_type()} ${raw(p)}) -> string {
                ${raw(show_format(t, p))}
            }
        }
    };
}

// `(a.x == b.x) && (a.y == b.y)`, or `true` with no fields.
fn eq_fields(Vec<string> names, string a, string b, bool tuple) -> string {
    if len(names) == 0 {
        return "true";
    }
    let out = "";
    let i = 0;
    while i < len(names) {
        let l = a + "." + names[i];
        let r = b + "." + names[i];
        if tuple {
            l = "a_" + names[i];
            r = "b_" + names[i];
        }
        let eq = "(" + l + " == " + r + ")";
        if i == 0 {
            out = eq;
        } else {
            out = "(" + out + " && " + eq + ")";
        }
        i += 1;
    }
    return out;
}

/// `impl Eq`: field-wise / variant-wise equality; `ne` is `!(a == b)`.
derive Eq(TypeDecl t) -> Code {
    let a = "__eq_a_" + t.name.str();
    let b = "__eq_b_" + t.name.str();
    let body = "";
    if t.repr != "" {
        let al = "__eq_al_" + t.name.str();
        let bl = "__eq_bl_" + t.name.str();
        body = "let " + al + ": " + t.repr + " = " + a + ";\nlet " + bl + ": " + t.repr + " = " + b
            + ";\nreturn " + al + " == " + bl + ";";
    } else if t.is_class() {
        let names: Vec<string> = Vec::new();
        for f in t.fields() {
            names.push(f.name.str());
        }
        body = "return " + eq_fields(names, a, b, false) + ";";
    } else {
        let arms = "";
        for v in t.variants() {
            let names = payload_names(v);
            let inner = pattern(t, v, "b_") + " => " + eq_fields(names, a, b, v.is_tuple()) + ",\ndefault => false,";
            arms += pattern(t, v, "a_") + " => match " + b + " {\n" + inner + "\n},\n";
        }
        body = "return match " + a + " {\n" + arms + "default => false,\n};";
    }
    return quote items {
        impl Eq for ${t.impl_head("Eq")} {
            fn eq(${t.self_type()} ${raw(a)}, ${t.self_type()} ${raw(b)}) -> bool {
                ${raw(body)}
            }
            fn ne(${t.self_type()} ${raw(a)}, ${t.self_type()} ${raw(b)}) -> bool {
                return !(${raw(a)} == ${raw(b)});
            }
        }
    };
}

// Lexicographic compare: `(a.x < b.x) || ((a.x == b.x) && (… || ((…) && eq)))`.
fn ord_chain(Vec<string> ls, Vec<string> rs, string op, bool eq_result) -> string {
    let acc = "false";
    if eq_result {
        acc = "true";
    }
    let i = len(ls) - 1;
    while i >= 0 {
        acc = "((" + ls[i] + " " + op + " " + rs[i] + ") || ((" + ls[i] + " == " + rs[i] + ") && " + acc + "))";
        i -= 1;
    }
    return acc;
}

fn ord_impl(TypeDecl t, string trait_name, string method, string op, string scalar_op, bool eq_result) -> string {
    let a = "__ord_" + method + "_a_" + t.name.str();
    let b = "__ord_" + method + "_b_" + t.name.str();
    let body = "";
    // A numeric backing orders by value; a string backing has no ordering
    // (strings are not ordered), so `#[repr(string)]` enums order by
    // declaration like any other enum.
    if t.repr != "" && t.repr != "string" {
        let al = "__ord_" + method + "_al_" + t.name.str();
        let bl = "__ord_" + method + "_bl_" + t.name.str();
        body = "let " + al + ": " + t.repr + " = " + a + ";\nlet " + bl + ": " + t.repr + " = " + b
            + ";\nreturn " + al + " " + scalar_op + " " + bl + ";";
    } else if t.is_class() {
        let ls: Vec<string> = Vec::new();
        let rs: Vec<string> = Vec::new();
        for f in t.fields() {
            ls.push(a + "." + f.name.str());
            rs.push(b + "." + f.name.str());
        }
        body = "return " + ord_chain(ls, rs, op, eq_result) + ";";
    } else {
        // Left tag below right: `<` / `<=` hold; above: `>` / `>=` hold.
        let left_less = op == "<";
        let arms = "";
        let vs = t.variants();
        let i = 0;
        while i < len(vs) {
            let inner = "";
            let j = 0;
            while j < len(vs) {
                let value = "";
                if j == i {
                    let names = payload_names(vs[i]);
                    let ls: Vec<string> = Vec::new();
                    let rs: Vec<string> = Vec::new();
                    for n in names {
                        ls.push(payload_value(vs[i], a, "a_", n));
                        rs.push(payload_value(vs[i], b, "b_", n));
                    }
                    value = ord_chain(ls, rs, op, eq_result);
                    inner += pattern(t, vs[j], "b_") + " => " + value + ",\n";
                } else {
                    if (j > i) == left_less {
                        value = "true";
                    } else {
                        value = "false";
                    }
                    inner += pattern(t, vs[j], "") + " => " + value + ",\n";
                }
                j += 1;
            }
            arms += pattern(t, vs[i], "a_") + " => match " + b + " {\n" + inner + "default => false,\n},\n";
            i += 1;
        }
        body = "return match " + a + " {\n" + arms + "default => false,\n};";
    }
    return "impl " + trait_name + " for " + t.impl_head("Ord + Eq").src() + " {\nfn " + method + "("
        + t.self_type().src() + " " + a + ", " + t.self_type().src() + " " + b + ") -> bool {\n" + body
        + "\n}\n}\n";
}

/// `impl Lt/Le/Gt/Ge` (lexicographic over fields, then variant order) and `Ord`.
derive Ord(TypeDecl t) -> Code {
    let impls = ord_impl(t, "Lt", "lt", "<", "<", false) + ord_impl(t, "Le", "le", "<", "<=", true)
        + ord_impl(t, "Gt", "gt", ">", ">", false) + ord_impl(t, "Ge", "ge", ">", ">=", true);
    return quote items {
        ${raw(impls)}
        impl Ord for ${t.impl_head("Ord + Eq")} {
        }
    };
}

// `((a.hash() * 31) + b.hash())`, from `seed` (the variant index).
fn hash_chain(string seed, Vec<string> values) -> string {
    let acc = seed;
    for v in values {
        if acc == "0" {
            // `0 * 31 + h` is `h`.
            acc = v + ".hash()";
        } else {
            acc = "((" + acc + " * 31) + " + v + ".hash())";
        }
    }
    return acc;
}

/// `impl Hash`: combines field / payload hashes; enums start from the variant index.
derive Hash(TypeDecl t) -> Code {
    let p = "__hash_" + t.name.str();
    let body = "";
    if t.repr != "" {
        let n = "__hash_n_" + t.name.str();
        body = "let " + n + ": " + t.repr + " = " + p + ";\nreturn " + n + ".hash();";
    } else if t.is_class() {
        let values: Vec<string> = Vec::new();
        for f in t.fields() {
            values.push(p + "." + f.name.str());
        }
        body = "return " + hash_chain("0", values) + ";";
    } else {
        let arms = "";
        let tag = 0;
        for v in t.variants() {
            let values: Vec<string> = Vec::new();
            for n in payload_names(v) {
                values.push(payload_value(v, p, "h_", n));
            }
            arms += pattern(t, v, "h_") + " => " + hash_chain(format_int(tag), values) + ",\n";
            tag += 1;
        }
        body = "return match " + p + " {\n" + arms + "default => 0,\n};";
    }
    return quote items {
        impl Hash for ${t.impl_head("Hash")} {
            fn hash(${t.self_type()} ${raw(p)}) -> int {
                ${raw(body)}
            }
        }
    };
}

/// Default value expression for a field or payload of type `ty`: a literal
/// for the primitives, `Ty::default()` for anything else.
fn default_value_for(TypeRef ty) -> string {
    let name = ty.str();
    if name == "int" || name == "byte" {
        return "0";
    }
    if name == "float" {
        return "0.0";
    }
    if name == "bool" {
        return "false";
    }
    if name == "string" {
        return "\"\"";
    }
    return name + "::default()";
}

/// `impl Default`: every class field takes its type's default (`0`, `0.0`,
/// `false`, `""`, or `Ty::default()`); an enum takes its first variant with
/// defaulted payloads.
derive Default(TypeDecl t) -> Code {
    let value = "";
    if t.is_class() {
        let args = "";
        let i = 0;
        for f in t.fields() {
            if i > 0 {
                args += ", ";
            }
            args += default_value_for(f.ty);
            i += 1;
        }
        value = "new " + t.name.str() + "(" + args + ")";
    } else if len(t.variants()) == 0 {
        value = "0";
    } else {
        let v = t.variants()[0];
        value = t.name.str() + "::" + v.name.str();
        if v.is_tuple() {
            let args = "";
            let i = 0;
            while i < len(v.tuple) {
                if i > 0 {
                    args += ", ";
                }
                args += default_value_for(v.tuple[i]);
                i += 1;
            }
            value += "(" + args + ")";
        } else if v.is_record() {
            let args = "";
            let i = 0;
            while i < len(v.fields) {
                if i > 0 {
                    args += ", ";
                }
                args += v.fields[i].name.str() + ": " + default_value_for(v.fields[i].ty);
                i += 1;
            }
            value += " { " + args + " }";
        }
    }
    return quote items {
        impl Default for ${t.impl_head("Default")} {
            static fn default() -> ${t.self_type()} {
                return ${raw(value)};
            }
        }
    };
}

/// Marker: the type may cross threads.
derive Send(TypeDecl t) -> Code {
    return quote items {
        impl Send for ${t.impl_head("Send")} {
        }
    };
}

/// Marker: the type holds sensitive data.
derive Sensitive(TypeDecl t) -> Code {
    return quote items {
        impl Sensitive for ${t.impl_head("Sensitive")} {
        }
    };
}
