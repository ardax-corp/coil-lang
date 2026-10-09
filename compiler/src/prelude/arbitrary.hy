// Random values for property tests: `Gen`, the `Arbitrary` trait, `any`
// and `#[derive(Arbitrary)]` for classes and enums. `coil test` uses them
// to call functions that have contracts. Embedded in the compiler as module
// `arbitrary`; see docs/internals/contracts.md.
use macro::{TypeDecl, Variant, TypeRef, Code, raw};
use task::{scope, Scope, TaskError};

// A seeded source of random values. `size` bounds how big values get
// (ints in `[-size, size]`, lengths up to `size`); `coil test` grows it
// from 0 so the first failures it finds are small.
class Gen {
    state: int,
    size: int,
    depth: int,
}

// xorshift32 steps, kept below 2^32 so no product or shift overflows.
fn mask32() -> int {
    return 4294967295;
}

impl Gen {
    pub static fn new(int seed) -> Gen {
        let s = seed & mask32();
        if s == 0 {
            s = 2463534242;
        }
        return new Gen(s, 10, 0);
    }

    // The current size bound.
    pub fn size() -> int {
        return self.size;
    }

    pub fn resize(int size) {
        if size < 0 {
            size = 0;
        }
        self.size = size;
    }

    // 32 random bits.
    pub fn bits() -> int {
        let x = self.state;
        x = x ^ ((x << 13) & mask32());
        x = x ^ (x >> 17);
        x = x ^ ((x << 5) & mask32());
        self.state = x;
        return x;
    }

    // Uniform in `[0, n)`; 0 when `n <= 0`.
    pub fn below(int n) -> int {
        if n <= 0 {
            return 0;
        }
        if n <= mask32() {
            return self.bits() % n;
        }
        return ((self.bits() << 31) | (self.bits() >> 1)) % n;
    }

    // Uniform in `[lo, hi]`.
    pub fn int_in(int lo, int hi) -> int {
        if hi <= lo {
            return lo;
        }
        return lo + self.below(hi - lo + 1);
    }

    // True one time in `n`.
    pub fn one_in(int n) -> bool {
        return self.below(n) == 0;
    }

    // How many elements a collection gets: up to `size`, none once the
    // value is nested `deep()`.
    pub fn len() -> int {
        if self.deep() {
            return 0;
        }
        return self.below(self.size + 1);
    }

    // How deep the value being built is nested in others.
    pub fn depth() -> int {
        return self.depth;
    }

    // Around the parts of a value. Once `deep()`, collections are empty and
    // derived enums take a variant without payloads, so recursive types end.
    pub fn enter() {
        self.depth += 1;
    }

    pub fn leave() {
        self.depth -= 1;
    }

    // Past this depth derived enums stop nesting.
    pub fn deep() -> bool {
        return self.depth >= 4;
    }
}

// A type `coil test` can make values of.
trait Arbitrary<T> {
    static fn arbitrary(Gen g) -> T {}
}

// A random `T`, chosen by the expected type: `let x: Vec<int> = any(g);`.
fn any<T: Arbitrary>(Gen g) -> T {
    return T::arbitrary(g);
}

impl Arbitrary for int {
    pub static fn arbitrary(Gen g) -> int {
        // Mostly small, sometimes an edge value.
        // No huge edge values: an overflow in the code under test aborts
        // the VM instead of failing one case.
        if g.one_in(8) {
            let edges = [0, 1, -1, g.size(), 0 - g.size()];
            return edges[g.below(5)];
        }
        return g.int_in(0 - g.size(), g.size());
    }
}

impl Arbitrary for byte {
    pub static fn arbitrary(Gen g) -> byte {
        let n = g.below(256);
        return n as byte;
    }
}

impl Arbitrary for bool {
    pub static fn arbitrary(Gen g) -> bool {
        return g.below(2) == 1;
    }
}

impl Arbitrary for float {
    pub static fn arbitrary(Gen g) -> float {
        let whole = g.int_in(0 - g.size(), g.size()) as float;
        let frac = (g.below(1000) as float) / 1000.0;
        return whole + frac;
    }
}

impl Arbitrary for string {
    pub static fn arbitrary(Gen g) -> string {
        let n = g.len();
        let out = "";
        let i = 0;
        while i < n {
            // Printable ASCII, now and then a two-byte character.
            if g.one_in(10) {
                out += "é";
            } else {
                let code = 32 + g.below(95);
                let c = code as byte;
                out += match char(c) {
                    Result::Ok(ch) => ch,
                    Result::Err(_) => "?",
                };
            }
            i += 1;
        }
        return out;
    }
}

impl Arbitrary for Vec<T: Arbitrary> {
    pub static fn arbitrary(Gen g) -> Vec<T> {
        let n = g.len();
        let out: Vec<T> = Vec::new();
        g.enter();
        let i = 0;
        while i < n {
            out.push(T::arbitrary(g));
            i += 1;
        }
        g.leave();
        return out;
    }
}

impl Arbitrary for Option<T: Arbitrary> {
    pub static fn arbitrary(Gen g) -> Option<T> {
        if g.one_in(4) {
            return Option::None;
        }
        return Option::Some(T::arbitrary(g));
    }
}

// What generated contract tests (`coil test`) use around each call.

// Call `body` in a child task: the panic message if it panicked.
fn run_case(unit -> unit body) -> Option<string> {
    let r = scope(fn (Scope s) use (body) {
        s.spawn(body);
    });
    return match r {
        Result::Ok(_) => Option::None,
        Result::Err(TaskError::Panicked(m)) => Option::Some(m),
        Result::Err(_) => Option::Some("the call was cancelled"),
    };
}

// `"text"` as written in source.
fn quote(string s) -> string {
    return "\"" + s + "\"";
}

// `[1, 2, 3]`.
fn show_vec<T: Show>(Vec<T> v) -> string {
    let out = "[";
    let i = 0;
    for x in v {
        if i > 0 {
            out += ", ";
        }
        out += x.show();
        i += 1;
    }
    return out + "]";
}

// `Some(1)` / `None`.
fn show_option<T: Show>(Option<T> o) -> string {
    return match o {
        Option::Some(x) => "Some(" + x.show() + ")",
        Option::None => "None",
    };
}

// `let __arb_<prefix><i>: <ty> = arbitrary::any(__arb_g);` for each type.
fn draw(Vec<TypeRef> tys, string prefix) -> string {
    let out = "";
    let i = 0;
    for ty in tys {
        out += "let __arb_" + prefix + string::format("%i", i) + ": " + ty.str() +
               " = arbitrary::any(__arb_g);\n";
        i += 1;
    }
    return out;
}

// `__arb_<prefix>0, __arb_<prefix>1, …`, or `name: __arb_<prefix>0, …`.
fn drawn(Vec<string> names, string prefix) -> string {
    let out = "";
    let i = 0;
    while i < len(names) {
        if i > 0 {
            out += ", ";
        }
        if names[i] != "" {
            out += names[i] + ": ";
        }
        out += "__arb_" + prefix + string::format("%i", i);
        i += 1;
    }
    return out;
}

// The value of variant `v` built from drawn payloads.
fn variant_value(TypeDecl t, Variant v, string prefix) -> string {
    let head = t.name.str() + "::" + v.name.str();
    let names: Vec<string> = Vec::new();
    if v.is_tuple() {
        for _ in v.tuple {
            names.push("");
        }
        return head + "(" + drawn(names, prefix) + ")";
    }
    if v.is_record() {
        for f in v.fields {
            names.push(f.name.str());
        }
        return head + " { " + drawn(names, prefix) + " }";
    }
    return head;
}

fn variant_types(Variant v) -> Vec<TypeRef> {
    if v.is_tuple() {
        return v.tuple;
    }
    let out: Vec<TypeRef> = Vec::new();
    for f in v.fields {
        out.push(f.ty);
    }
    return out;
}

/// `impl Arbitrary`: a class gets an arbitrary value per field; an enum picks
/// a variant (one without payloads once the value is nested deep) and fills
/// its payloads. A class with an `invariant` needs a hand-written instance:
/// a derived value may break it.
derive Arbitrary(TypeDecl t) -> Code {
    let body = "";
    if t.is_class() {
        let tys: Vec<TypeRef> = Vec::new();
        let names: Vec<string> = Vec::new();
        for f in t.fields() {
            tys.push(f.ty);
            names.push("");
        }
        body = "__arb_g.enter();\n" + draw(tys, "f") + "__arb_g.leave();\nreturn new " +
               t.name.str() + "(" + drawn(names, "f") + ");";
    } else {
        let vs = t.variants();
        let leaf = 0;
        let found = false;
        let i = 0;
        while i < len(vs) {
            if !found && !vs[i].is_tuple() && !vs[i].is_record() {
                leaf = i;
                found = true;
            }
            i += 1;
        }
        let leaf_text = string::format("%i", leaf);
        body = "let __arb_k = __arb_g.below(" + string::format("%i", len(vs)) + ");\n" +
               "if __arb_g.deep() {\n__arb_k = " + leaf_text + ";\n}\n__arb_g.enter();\n";
        i = 0;
        while i < len(vs) {
            let v = vs[i];
            if i != leaf {
                body += "if __arb_k == " + string::format("%i", i) + " {\n" +
                        draw(variant_types(v), "p") + "__arb_g.leave();\nreturn " +
                        variant_value(t, v, "p") + ";\n}\n";
            }
            i += 1;
        }
        let lv = vs[leaf];
        body += draw(variant_types(lv), "p") + "__arb_g.leave();\nreturn " +
                variant_value(t, lv, "p") + ";";
    }
    return quote items {
        impl Arbitrary for ${t.impl_head("Arbitrary")} {
            pub static fn arbitrary(Gen __arb_g) -> ${t.self_type()} {
                ${raw(body)}
            }
        }
    };
}
