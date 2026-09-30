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

/// Wrap a function so it returns `result + by`.
attr add_after(FnDecl f, int by) -> Code {
    let inner = ident(f.name.str() + "__inner");
    return quote items {
        fn ${raw(f.signature(inner.str()))} ${raw(f.body_source())}
        fn ${raw(f.signature(f.name.str()))} {
            let r = ${inner}(${raw(f.arg_names())});
            return r + ${lit_int(by)};
        }
    };
}
