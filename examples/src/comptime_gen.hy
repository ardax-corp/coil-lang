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
