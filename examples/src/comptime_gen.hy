// Code generators that `comptime { … }` blocks call, for `tests/comptime/`.
use macro::{Code, raw};

/// `fn NAME(int x) -> int`, adding `n`.
fn adder(string name, int n) -> Code {
    return raw("fn " + name + "(int x) -> int {\n    return x + " + n.show() + ";\n}\n");
}

/// A class per name, each with an `id` field and a `name` method.
fn records(Vec<string> names) -> Code {
    let text = "";
    for name in names {
        if name != "" {
            text = text + "class " + name + " {\n    pub id: int,\n}\n\nimpl " + name +
                   " {\n    pub fn name() -> string {\n        return \"" + name +
                   "\";\n    }\n}\n\n";
        }
    }
    return raw(text);
}

/// `macro NAME(Expr e) -> Code`: `e` times `n`.
fn scale_macro(string name, int n) -> Code {
    return raw(
        "macro " + name + "(Expr e) -> Code {\n    return quote expr { ($" + "{e}) * " + n.show() +
        " };\n}\n",
    );
}

/// `derive NAME`: `T::field_count()`, the number of the type's fields.
fn count_derive(string name) -> Code {
    return raw(
        "derive " + name + "(TypeDecl t) -> Code {\n    return quote items {\n        impl $" +
        "{t.name} {\n            pub static fn field_count() -> int {\n                return $" +
        "{lit_int(t.fields().len())};\n            }\n        }\n    };\n}\n",
    );
}
