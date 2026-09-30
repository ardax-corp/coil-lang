// Provider for tests/positive/derive_macro_*.hy: user derive and attribute
// macros, run at compile time.
use macro::{TypeDecl, FnDecl, Code, lit, lit_int, ident, raw};

/// `T::field_names()` lists the fields, honouring `#[names(rename = "…")]`.
derive FieldNames(TypeDecl t) -> Code attrs(names) {
    let names: Vec<Code> = Vec::new();
    for f in t.fields() {
        names.push(lit(f.attr_str("names", "rename", f.name.str())));
    }
    return quote items {
        impl ${t.name} {
            pub static fn field_names() -> Vec<string> {
                let out: Vec<string> = Vec::from([$(names),*]);
                return out;
            }
        }
    };
}

/// `impl Named for E` for an enum: `variant_name()` returns the variant's
/// name. The `Named` trait lives in the using module.
derive VariantName(TypeDecl t) -> Code {
    let arms: Vec<Code> = Vec::new();
    for v in t.variants() {
        let pat = t.name.str() + "::" + v.name.str();
        if v.arity() > 0 {
            let holes = "_";
            let i = 1;
            while i < v.arity() {
                holes += ", _";
                i += 1;
            }
            pat += "(" + holes + ")";
        }
        arms.push(quote stmts { ${raw(pat)} => ${lit(v.name.str())}, });
    }
    return quote items {
        impl Named for ${t.name} {
            pub fn variant_name(${t.name} self) -> string {
                return match self {
                    $(arms)*
                };
            }
        }
    };
}

/// Wrap a function so it returns `result + by` (its other attributes stay on
/// the wrapped body, so stacked macros compose).
attr add_after(FnDecl f, int by) -> Code {
    let inner = ident(f.name.str() + "__inner");
    return quote items {
        ${raw(f.with_name(inner.str()))}
        fn ${raw(f.signature(f.name.str()))} {
            let r = ${inner}(${raw(f.arg_names())});
            return r + ${lit_int(by)};
        }
    };
}

/// Returned by `Tagged` derives; generated code names it `derive_macros::Tag`.
class Tag {
    pub label: string,
}

fn tag_prefix() -> string {
    return "tag:";
}

/// `T::tag()` builds a provider `Tag` through a provider helper, neither of
/// which the using module imports.
derive Tagged(TypeDecl t) -> Code {
    return quote items {
        impl ${t.name} {
            pub static fn tag() -> Tag {
                let label = tag_prefix() + ${lit(t.name.str())};
                return new Tag(label);
            }
        }
    };
}

/// Keep a class and add `describe()`: `#[with_describe(prefix = "P")]`.
attr with_describe(TypeDecl t, string prefix) -> Code {
    return quote items {
        ${raw(t.source)}
        impl ${t.name} {
            pub fn describe() -> string {
                return ${lit(prefix + ":" + t.name.str())};
            }
        }
    };
}

/// On a method: keep it and add `<name>_twice()` returning it summed twice.
attr twice(FnDecl f) -> Code {
    let twice = ident(f.name.str() + "_twice");
    return quote items {
        pub ${raw(f.source)}
        pub fn ${twice}() -> int {
            return self.${f.name}() + self.${f.name}();
        }
    };
}

/// A user derive may implement a prelude trait; it replaces the compiler's
/// default type-name `Show`.
derive Loud(TypeDecl t) -> Code {
    return quote items {
        impl Show for ${t.name} {
            pub fn show(${t.name} self) -> string {
                return ${lit("LOUD " + t.name.str())};
            }
        }
    };
}
