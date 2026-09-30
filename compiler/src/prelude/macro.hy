// Compile-time declaration model for derive and attribute macros.
//
// A derive receives a read-only `TypeDecl` describing the declaration as
// written (types are not resolved) and returns `Code`: coil source that is
// parsed and typechecked like hand-written code, placed after the type. An
// attribute macro receives an `FnDecl` or `TypeDecl` and returns the `Code`
// that replaces it.
//
// `quote items|expr|stmts|type { … }` builds `Code`. `${x}` splices
// `x.src()` (an `Ident`, `TypeRef` or `Code`; wrap strings with `lit`), and
// `$(xs) sep *` splices a `Vec<Code>` with `sep` in between.
use string::{format, to_bytes, from_bytes};

/// Generated coil source: declarations, an expression, statements or a type.
class Code {
    pub text: string,
}

impl Code {
    pub fn src() -> string {
        return self.text;
    }
}

/// Join fragments with `sep` (the lowering of `$(xs) sep *`).
fn join(Vec<Code> xs, string sep) -> string {
    let out = "";
    let i = 0;
    while i < len(xs) {
        if i > 0 {
            out += sep;
        }
        out += xs[i].text;
        i += 1;
    }
    return out;
}

/// A string literal: `lit("a\"b")` splices as `"a\"b"`.
fn lit(string s) -> Code {
    let out: Vec<byte> = Vec::new();
    out.push(34 as byte);
    for b in to_bytes(s) {
        if b == 34 as byte || b == 92 as byte {
            out.push(92 as byte);
        }
        out.push(b);
    }
    out.push(34 as byte);
    return match from_bytes(out) {
        Result::Ok(text) => new Code(text),
        Result::Err(_) => new Code("\"\""),
    };
}

/// An integer literal.
fn lit_int(int n) -> Code {
    return new Code(format("%i", n));
}

/// Raw source, spliced as is.
fn raw(string text) -> Code {
    return new Code(text);
}

/// Combine several generated item lists.
fn concat(Vec<Code> parts) -> Code {
    return new Code(join(parts, "\n"));
}

/// A name as written in source.
class Ident {
    pub name: string,
}

impl Ident {
    pub fn str() -> string {
        return self.name;
    }

    pub fn src() -> string {
        return self.name;
    }
}

/// A name, for splicing generated identifiers: `${ident("to_" + n)}`.
fn ident(string name) -> Ident {
    return new Ident(name);
}

/// A type annotation as written (not resolved): `Vec<int>`, `json::Value`.
class TypeRef {
    pub text: string,
    pub head_name: string,
    pub arg_refs: Vec<TypeRef>,
}

impl TypeRef {
    /// The whole annotation, e.g. `Map<string, int>`.
    pub fn str() -> string {
        return self.text;
    }

    pub fn src() -> string {
        return self.text;
    }

    /// The type constructor, e.g. `Map`.
    pub fn head() -> string {
        return self.head_name;
    }

    /// Generic arguments, e.g. `[string, int]`.
    pub fn args() -> Vec<TypeRef> {
        return self.arg_refs;
    }
}

/// One `key = value` (or positional `value`, with `key == ""`) attribute argument.
class AttrArg {
    pub key: string,
    /// Literal value as written (strings without their quotes).
    pub value: string,
    /// `"string"`, `"int"`, `"float"`, `"bool"` or `"ident"`.
    pub kind: string,
}

/// `#[name(args)]` on a declaration, field or variant.
class Attr {
    pub name: string,
    pub args: Vec<AttrArg>,
}

impl Attr {
    /// The attribute as written: `#[name(key = "v", 2)]`.
    pub fn src() -> string {
        if len(self.args) == 0 {
            return "#[" + self.name + "]";
        }
        let out = "#[" + self.name + "(";
        let i = 0;
        while i < len(self.args) {
            let a = self.args[i];
            if i > 0 {
                out += ", ";
            }
            if a.key != "" {
                out += a.key + " = ";
            }
            if a.kind == "string" {
                out += lit(a.value).text;
            } else {
                out += a.value;
            }
            i += 1;
        }
        return out + ")]";
    }

    /// True when the attribute has an argument named `key`, or a bare `key`.
    pub fn has(string key) -> bool {
        for a in self.args {
            if a.key == key || (a.key == "" && a.value == key) {
                return true;
            }
        }
        return false;
    }

    /// Value of the `key = value` argument, or `fallback`.
    pub fn arg(string key, string fallback) -> string {
        for a in self.args {
            if a.key == key {
                return a.value;
            }
        }
        return fallback;
    }
}

/// Index of the attribute called `name`, or `-1`.
fn attr_index(Vec<Attr> attrs, string name) -> int {
    let i = 0;
    while i < len(attrs) {
        if attrs[i].name == name {
            return i;
        }
        i += 1;
    }
    return -1;
}

/// `#[attr(key = "value")]` from an attribute list, or `fallback`.
fn attr_value(Vec<Attr> attrs, string attr, string key, string fallback) -> string {
    let i = attr_index(attrs, attr);
    if i < 0 {
        return fallback;
    }
    let a = attrs[i];
    return a.arg(key, fallback);
}

/// A class field, or a field of a record enum variant.
class Field {
    pub name: Ident,
    pub ty: TypeRef,
    pub is_pub: bool,
    pub attrs: Vec<Attr>,
    pub docs: Vec<string>,
}

impl Field {
    /// True when the field carries `#[name(...)]`.
    pub fn has_attr(string name) -> bool {
        return attr_index(self.attrs, name) >= 0;
    }

    /// `#[attr(key = "value")]` on this field, or `fallback`.
    pub fn attr_str(string attr, string key, string fallback) -> string {
        return attr_value(self.attrs, attr, key, fallback);
    }
}

/// One enum variant. `shape` is `"unit"`, `"tuple"` (payload in `tuple`) or
/// `"record"` (payload in `fields`).
class Variant {
    pub name: Ident,
    pub shape: string,
    pub tuple: Vec<TypeRef>,
    pub fields: Vec<Field>,
    /// Scalar discriminant as written (`200`, `"ok"`), or `""`.
    pub value: string,
    pub attrs: Vec<Attr>,
    pub docs: Vec<string>,
}

impl Variant {
    pub fn is_unit() -> bool {
        return self.shape == "unit";
    }

    pub fn is_tuple() -> bool {
        return self.shape == "tuple";
    }

    pub fn is_record() -> bool {
        return self.shape == "record";
    }

    /// Number of payload values (0 for a unit variant).
    pub fn arity() -> int {
        if self.shape == "tuple" {
            return len(self.tuple);
        }
        return len(self.fields);
    }

    pub fn has_attr(string name) -> bool {
        return attr_index(self.attrs, name) >= 0;
    }

    pub fn attr_str(string attr, string key, string fallback) -> string {
        return attr_value(self.attrs, attr, key, fallback);
    }
}

/// A class or enum declaration handed to a derive or attribute macro.
class TypeDecl {
    pub name: Ident,
    /// `"class"` or `"enum"`.
    pub kind: string,
    pub generics: Vec<Ident>,
    pub field_list: Vec<Field>,
    pub variant_list: Vec<Variant>,
    pub attrs: Vec<Attr>,
    /// `#[repr(...)]` backing (`"int"`, …) or the inferred scalar backing, else `""`.
    pub repr: string,
    /// Module path the declaration lives in (`""` for the entry file).
    pub module: string,
    pub docs: Vec<string>,
    /// The declaration as written (an attribute macro's own attribute removed).
    pub source: string,
}

impl TypeDecl {
    pub fn is_class() -> bool {
        return self.kind == "class";
    }

    pub fn is_enum() -> bool {
        return self.kind == "enum";
    }

    /// Class fields (empty for an enum).
    pub fn fields() -> Vec<Field> {
        return self.field_list;
    }

    /// Enum variants (empty for a class).
    pub fn variants() -> Vec<Variant> {
        return self.variant_list;
    }

    pub fn has_attr(string name) -> bool {
        return attr_index(self.attrs, name) >= 0;
    }

    pub fn attr_str(string attr, string key, string fallback) -> string {
        return attr_value(self.attrs, attr, key, fallback);
    }
}

/// One function parameter.
class Param {
    pub name: Ident,
    pub ty: TypeRef,
}

/// A function or method handed to an attribute macro.
class FnDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    /// Return type as written, or `unit` when the function declares none.
    pub ret: TypeRef,
    pub type_params: Vec<Ident>,
    /// Remaining attributes (the macro's own attribute is removed).
    pub attrs: Vec<Attr>,
    /// Owning class for an `impl` method, else `""`.
    pub owner: string,
    /// `pub` (methods; top-level functions are always visible).
    pub is_pub: bool,
    pub is_static: bool,
    pub is_coro: bool,
    pub docs: Vec<string>,
    /// The body block as written, braces included.
    pub body: string,
    /// The whole declaration as written (the macro's own attribute removed).
    pub source: string,
}

impl FnDecl {
    pub fn body_source() -> string {
        return self.body;
    }

    /// `name(T a, U b) -> R`: the signature as written, under a new name.
    pub fn signature(string name) -> string {
        let out = name + "(";
        let i = 0;
        while i < len(self.params) {
            if i > 0 {
                out += ", ";
            }
            out += self.params[i].ty.text + " " + self.params[i].name.name;
            i += 1;
        }
        out += ")";
        if self.ret.text != "unit" {
            out += " -> " + self.ret.text;
        }
        return out;
    }

    /// The function under another name, with its remaining attributes:
    /// `#[other] pub fn name(T a) -> R { body }`.
    pub fn with_name(string name) -> string {
        let out = "";
        for a in self.attrs {
            out += a.src() + "\n";
        }
        if self.is_pub {
            out += "pub ";
        }
        if self.is_coro {
            out += "async ";
        }
        if self.is_static {
            out += "static ";
        }
        return out + "fn " + self.signature(name) + " " + self.body;
    }

    /// A call of `name` with this function's arguments (`self.name(a, b)`
    /// for an instance method).
    pub fn call(string name) -> string {
        let prefix = "";
        if self.owner != "" && !self.is_static {
            prefix = "self.";
        }
        return prefix + name + "(" + self.arg_names() + ")";
    }

    /// `a, b`: the parameter names, for forwarding a call.
    pub fn arg_names() -> string {
        let out = "";
        let i = 0;
        while i < len(self.params) {
            if i > 0 {
                out += ", ";
            }
            out += self.params[i].name.name;
            i += 1;
        }
        return out;
    }
}
