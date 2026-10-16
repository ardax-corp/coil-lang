// An overloaded function bound to a `let` annotated with a function type
// takes the overload that matches the annotation (coil-lang#1155).
use tests::assert;

fn show(int x) -> string {
    return "i";
}

fn show(float x) -> string {
    return "f";
}

test("annotation picks the int overload") {
    let g: int -> string = show;
    assert!(g(4) == "i");
}

test("annotation picks the float overload") {
    let h: float -> string = show;
    assert!(h(1.5) == "f");
}
