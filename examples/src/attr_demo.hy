// Attribute macros for examples/attr_decorator.hy and attr_class.hy.
use macro::{FnDecl, TypeDecl, Code, lit, raw, ident};
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

/// Write `text` to stdout (generated code calls `attr_demo::say`).
fn say(string text) {
    write_all(stdout(), to_bytes(format("%s", text)));
}

// `f` renamed to `inner`, and a wrapper under `f`'s name that says `text`
// before forwarding. Other attributes stay on the renamed function, so
// stacked macros apply outermost first.
fn wrap(FnDecl f, string text, string suffix) -> Code {
    let inner = f.name.str() + suffix;
    let head = "fn ";
    if f.is_pub {
        head = "pub fn ";
    }
    return quote items {
        ${raw(f.with_name(inner))}
        ${raw(head + f.signature(f.name.str()))} {
            say(${lit(text)});
            return ${raw(f.call(inner))};
        }
    };
}

/// Print `message`, then run the function.
attr log(FnDecl f, string message) -> Code {
    return wrap(f, message, "__log");
}

/// Print `metric`, then run the function.
attr measure(FnDecl f, string metric) -> Code {
    return wrap(f, metric, "__measure");
}

/// Keep a class and add `make(...)`, a constructor that prints `message`.
attr logged_new(TypeDecl t, string message) -> Code {
    let params = "";
    let args = "";
    let i = 0;
    for f in t.fields() {
        if i > 0 {
            params += ", ";
            args += ", ";
        }
        params += f.ty.str() + " " + f.name.str();
        args += f.name.str();
        i += 1;
    }
    return quote items {
        ${raw(t.source)}
        impl ${t.name} {
            pub static fn make(${raw(params)}) -> ${t.name} {
                say(${lit(message)});
                return new ${t.name}(${raw(args)});
            }
        }
    };
}
