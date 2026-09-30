// Misbehaving macros for tests/compile_fail/macro_*.hy.
use macro::{TypeDecl, Code, raw};
use env::var;

/// Never returns: stopped by the compile-time step budget.
derive Spin(TypeDecl t) -> Code {
    let n = 0;
    while n >= 0 {
        n += 1;
    }
    return raw("");
}

/// Reads the environment: host access is denied at compile time.
derive Peek(TypeDecl t) -> Code {
    let v = var("HOME");
    return raw("");
}

/// Panics with a message the user sees.
derive Refuse(TypeDecl t) -> Code {
    panic "Refuse cannot derive " + t.name.str();
}

/// Generates code that does not typecheck.
derive BadType(TypeDecl t) -> Code {
    return quote items {
        impl ${t.name} {
            pub fn broken() -> int {
                return "not an int";
            }
        }
    };
}

/// Generates code that does not parse.
derive BadSyntax(TypeDecl t) -> Code {
    return raw("impl {");
}

/// Same name as `derive_macros::FieldNames`.
derive FieldNames(TypeDecl t) -> Code {
    return raw("");
}
