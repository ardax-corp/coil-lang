// User derive / attribute macros from `examples/src/derive_macros.hy`.
use derive_macros::{FieldNames, VariantName, Tagged, Loud, add_after};

#[derive(FieldNames, Tagged)]
class Config {
    #[names(rename = "server-port")]
    pub port: int,
    pub name: string,
}

// Same-module trait: the derive's `impl Named for Shape` lands here.
trait Named<T> {
    fn variant_name(T self) -> string {}
}

#[derive(VariantName, Eq)]
enum Shape {
    Dot,
    Circle(int),
}

#[add_after(by = 10)]
fn triple(int x) -> int {
    return x * 3;
}

test("derive generates a static method with helper attributes") {
    let names = Config::field_names();
    assert(len(names) == 2)?;
    assert(names[0] == "server-port")?;
    assert(names[1] == "name")?;
}

test("derive over enum variants") {
    assert(Shape::Dot.variant_name() == "Dot")?;
    assert(Shape::Circle(2).variant_name() == "Circle")?;
    assert(Shape::Dot == Shape::Dot)?;
}

test("attribute macro replaces the function") {
    assert(triple(2) == 16)?;
}

test("generated code names provider items by path") {
    let t = Config::tag();
    assert(t.label == "tag:Config")?;
}

#[derive(Loud)]
class Quiet {
    pub x: int,
}

test("user derive of a prelude trait replaces the default Show") {
    let q = new Quiet(1);
    assert(q.show() == "LOUD Quiet")?;
}
