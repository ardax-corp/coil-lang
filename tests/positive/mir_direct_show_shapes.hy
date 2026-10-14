// `%v` and `show` shapes direct MIR lowers from HIR: tuples and records
// shown item by item, an error enum's `show`, and a method on a variant
// literal of a two-word enum (boxed for its boxed parameter).
use tests::assert;
use string::format;

#[derive(Show)]
enum Sh {
    Dot,
    Circle(int),
}

#[derive(Show)]
enum Fault {
    Bad(string),
    Worse(int),
}

fn tuple_text(int a, string b) -> string {
    return format("%v|%v", (a, b), ((a, true), 1.5));
}

fn record_text(int a) -> string {
    return format("%v", { x: a, y: "k" });
}

fn fault_text(Fault f) -> string {
    return format("got %v", f);
}

fn circle(int r) -> string {
    return Sh::Circle(r).show();
}

test("tuples and records") {
    assert!(tuple_text(3, "q") == "(3, q)|((3, true), 1.5)", tuple_text(3, "q"));
    assert!(record_text(4) == "{ x: 4, y: k }", record_text(4));
}

test("error enum") {
    assert!(fault_text(Fault::Bad("x")) == "got Fault::Bad(x)", fault_text(Fault::Bad("x")));
    assert!(fault_text(Fault::Worse(2)) == "got Fault::Worse(2)");
}

test("variant literal receiver") {
    assert!(circle(3) == "Sh::Circle(3)", circle(3));
}
