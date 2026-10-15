// A trailing comma is accepted after the last enum variant, class field,
// match arm, array item, call argument and parameter; empty and nested
// blocks are statements; an expression statement discards its value.
use tests::assert;

enum Light {
    Red,
    Green,
}

class Cell {
    pub v: int,
}

fn add(int a, int b) -> int {
    return a + b;
}

fn name(Light l) -> string {
    return match l {
        Light::Red => "red",
        Light::Green => "green",
    };
}

test("trailing commas") {
    let xs = [1, 2, 3];
    assert!(len(xs) == 3);
    assert!(add(1, 2) == 3);
    assert!(name(Light::Green) == "green");
    assert!(new Cell(4).v == 4);
    let t = (1, 2);
    assert!(len(t) == 2);
}

test("empty and nested blocks") {
    let x = 1;
    {}
    {
        {
            {
                x = x + 1;
            }
        }
    }
    assert!(x == 2);
}

test("expression statements discard their value") {
    let n = 0;
    add(1, 2);
    n + 1;
    n++;
    assert!(n == 1);
}
