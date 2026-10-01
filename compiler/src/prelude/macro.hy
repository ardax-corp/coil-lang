// Compile-time declaration model for derives, attribute macros and
// function-style macros.
//
// A derive receives a read-only `TypeDecl` describing the declaration as
// written (types are not resolved) and returns `Code`: coil source that is
// parsed and typechecked like hand-written code, placed after the type. An
// attribute macro receives an `FnDecl` or `TypeDecl` and returns the `Code`
// that replaces it. A function-style macro (`macro name(Expr a, …)`, used as
// `name!(…)`) receives each argument as an `Expr` and returns the `Code` that
// replaces the call: items, statements or one expression, by where the call
// is.
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

/// A function-style macro argument as written: `f(1)`, `a + b`, `"text"`.
class Expr {
    pub text: string,
    /// `literal`, `ident`, `path`, `call` or `other`.
    pub kind_name: string,
}

impl Expr {
    /// The source text as written.
    pub fn str() -> string {
        return self.text;
    }

    /// The source to splice: parenthesized unless it is a single term, so
    /// `${a} * 2` keeps `a` whole.
    pub fn src() -> string {
        if self.kind_name == "other" {
            return "(" + self.text + ")";
        }
        return self.text;
    }

    pub fn kind() -> string {
        return self.kind_name;
    }

    pub fn is_literal() -> bool {
        return self.kind_name == "literal";
    }

    pub fn is_ident() -> bool {
        return self.kind_name == "ident";
    }
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

    /// The type as written in a signature: `Name`, or `Name<T, U>` for a
    /// generic type.
    pub fn self_type() -> Code {
        if len(self.generics) == 0 {
            return new Code(self.name.str());
        }
        let out = self.name.str() + "<";
        let i = 0;
        while i < len(self.generics) {
            if i > 0 {
                out += ", ";
            }
            out += self.generics[i].str();
            i += 1;
        }
        return new Code(out + ">");
    }

    /// Head of an `impl Trait for …` for this type: `Name`, or for a generic
    /// type `Name<T: bound, U: bound>`, every type parameter bounded by
    /// `bound` (`"Show"`, `"Ord + Eq"`). Write `impl ${trait} for
    /// ${t.impl_head("Show")}` so a derive covers generic types too.
    pub fn impl_head(string bound) -> Code {
        if len(self.generics) == 0 {
            return new Code(self.name.str());
        }
        let out = self.name.str() + "<";
        let i = 0;
        while i < len(self.generics) {
            if i > 0 {
                out += ", ";
            }
            out += self.generics[i].str() + ": " + bound;
            i += 1;
        }
        return new Code(out + ">");
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

/// Reads a macro call's input: length-prefixed fields (`<len>:<bytes>`,
/// lists as a count then their items) in constructor order, written by the
/// compiler (`macros::encode`).
class Reader {
    pub bytes: Vec<byte>,
    pub pos: int,
}

// A free function: a method calling itself here resolved its return type
// without the module path.
fn read_type_ref(Reader r) -> TypeRef {
    let text = r.str();
    let head = r.str();
    let n = r.int();
    let args: Vec<TypeRef> = Vec::new();
    let i = 0;
    while i < n {
        let a = read_type_ref(r);
        args.push(a);
        i += 1;
    }
    return new TypeRef(text, head, args);
}

impl Reader {
    pub static fn over(string input) -> Reader {
        return new Reader(to_bytes(input), 0);
    }

    /// One field.
    pub fn str() -> string {
        let n = 0;
        while self.bytes[self.pos] != 58 as byte {
            n = n * 10 + (self.bytes[self.pos] as int - 48);
            self.pos += 1;
        }
        self.pos += 1;
        let out: Vec<byte> = Vec::new();
        let end = self.pos + n;
        while self.pos < end {
            out.push(self.bytes[self.pos]);
            self.pos += 1;
        }
        return match from_bytes(out) {
            Result::Ok(text) => text,
            Result::Err(_) => "",
        };
    }

    /// A field holding a decimal integer, possibly negative.
    pub fn int() -> int {
        let digits = to_bytes(self.str());
        let n = 0;
        let i = 0;
        let negative = false;
        if len(digits) > 0 && digits[0] == 45 as byte {
            negative = true;
            i = 1;
        }
        while i < len(digits) {
            n = n * 10 + (digits[i] as int - 48);
            i += 1;
        }
        if negative {
            return 0 - n;
        }
        return n;
    }

    pub fn bool() -> bool {
        return self.str() == "1";
    }

    pub fn strings() -> Vec<string> {
        let n = self.int();
        let out: Vec<string> = Vec::new();
        let i = 0;
        while i < n {
            let s = self.str();
            out.push(s);
            i += 1;
        }
        return out;
    }

    pub fn ident() -> Ident {
        let name = self.str();
        return new Ident(name);
    }

    /// A macro argument: kind, then text.
    pub fn expr() -> Expr {
        let kind = self.str();
        let text = self.str();
        return new Expr(text, kind);
    }

    pub fn exprs() -> Vec<Expr> {
        let n = self.int();
        let out: Vec<Expr> = Vec::new();
        let i = 0;
        while i < n {
            let x = self.expr();
            out.push(x);
            i += 1;
        }
        return out;
    }

    pub fn idents() -> Vec<Ident> {
        let n = self.int();
        let out: Vec<Ident> = Vec::new();
        let i = 0;
        while i < n {
            let x = self.ident();
            out.push(x);
            i += 1;
        }
        return out;
    }

    pub fn type_ref() -> TypeRef {
        return read_type_ref(self);
    }

    pub fn type_refs() -> Vec<TypeRef> {
        let n = self.int();
        let out: Vec<TypeRef> = Vec::new();
        let i = 0;
        while i < n {
            let t = self.type_ref();
            out.push(t);
            i += 1;
        }
        return out;
    }

    pub fn attrs() -> Vec<Attr> {
        let n = self.int();
        let out: Vec<Attr> = Vec::new();
        let i = 0;
        while i < n {
            let name = self.str();
            let argc = self.int();
            let args: Vec<AttrArg> = Vec::new();
            let j = 0;
            while j < argc {
                let key = self.str();
                let value = self.str();
                let kind = self.str();
                let arg = new AttrArg(key, value, kind);
                args.push(arg);
                j += 1;
            }
            let a = new Attr(name, args);
            out.push(a);
            i += 1;
        }
        return out;
    }

    pub fn field() -> Field {
        let name = self.ident();
        let ty = self.type_ref();
        let is_pub = self.bool();
        let attrs = self.attrs();
        let docs = self.strings();
        return new Field(name, ty, is_pub, attrs, docs);
    }

    pub fn fields() -> Vec<Field> {
        let n = self.int();
        let out: Vec<Field> = Vec::new();
        let i = 0;
        while i < n {
            let f = self.field();
            out.push(f);
            i += 1;
        }
        return out;
    }

    pub fn variants() -> Vec<Variant> {
        let n = self.int();
        let out: Vec<Variant> = Vec::new();
        let i = 0;
        while i < n {
            let name = self.ident();
            let shape = self.str();
            let tuple = self.type_refs();
            let fields = self.fields();
            let value = self.str();
            let attrs = self.attrs();
            let docs = self.strings();
            let v = new Variant(name, shape, tuple, fields, value, attrs, docs);
            out.push(v);
            i += 1;
        }
        return out;
    }

    pub fn type_decl() -> TypeDecl {
        let name = self.ident();
        let kind = self.str();
        let generics = self.idents();
        let fields = self.fields();
        let variants = self.variants();
        let attrs = self.attrs();
        let repr = self.str();
        let module = self.str();
        let docs = self.strings();
        let source = self.str();
        return new TypeDecl(
            name,
            kind,
            generics,
            fields,
            variants,
            attrs,
            repr,
            module,
            docs,
            source,
        );
    }

    pub fn fn_decl() -> FnDecl {
        let name = self.ident();
        let n = self.int();
        let params: Vec<Param> = Vec::new();
        let i = 0;
        while i < n {
            let pname = self.ident();
            let pty = self.type_ref();
            let p = new Param(pname, pty);
            params.push(p);
            i += 1;
        }
        let ret = self.type_ref();
        let type_params = self.idents();
        let attrs = self.attrs();
        let owner = self.str();
        let is_pub = self.bool();
        let is_static = self.bool();
        let is_coro = self.bool();
        let docs = self.strings();
        let body = self.str();
        let source = self.str();
        return new FnDecl(
            name,
            params,
            ret,
            type_params,
            attrs,
            owner,
            is_pub,
            is_static,
            is_coro,
            docs,
            body,
            source,
        );
    }
}
