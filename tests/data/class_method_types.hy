// Inherent methods take the receiver as an implicit `self`: a method's
// call type is its declared return, `pub` methods are callable from any
// function, and methods may read the receiver's fields.
use tests::assert;

class Foo {
    pub name: string,
}

impl Foo {
    pub fn id() -> Foo {
        return new Foo(self.name + "!");
    }

    pub fn method(int x) -> int {
        return x;
    }

    pub fn sadge() -> int {
        return 42;
    }
}

class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }
}

test("a method returning the owner type") {
    let f = new Foo("hi");
    assert!((typeof f.id()) == "Foo");
    assert!(f.id().name == "hi!");
}

test("explicit arguments follow the implicit self") {
    let f = new Foo("hi");
    assert!((typeof f.method(3)) == "int");
    assert!(f.method(3) == 3);
}

test("a pub method is callable outside its impl") {
    let f = new Foo("hi");
    assert!(f.sadge() == 42);
}

test("a method reads the receiver's fields") {
    let p = new Point(1, 3);
    assert!((typeof p.sum()) == "int");
    assert!(p.sum() == 4);
}
