// A `Vec`, array, tuple or record whose elements are a generic body's own
// type parameters meets a `T: Length` bound too: the shape's structural
// instance reads the same length whatever the elements are (#1542).
use tests::assert;

fn size<T: Length>(T x) -> int {
    return x.len();
}

fn vec_size<E>(Vec<E> v) -> int {
    return size(v);
}

fn array_size<E>(E e) -> int {
    return size([e, e, e]) + size([e]);
}

fn pair_size<A, B>(A a, B b) -> int {
    return size((a, b)) + size((a, b, a));
}

fn record_size<E>(E e) -> int {
    return size({ x: e, y: 1 });
}

test("an open Vec, array, tuple or record meets a Length bound") {
    assert!(vec_size(Vec::from([1, 2, 3])) == 3);
    assert!(vec_size(Vec::from(["a"])) == 1);
    assert!(array_size(1) == 4);
    assert!(array_size("a") == 4);
    assert!(pair_size(1, "a") == 5);
    assert!(record_size(2.5) == 2);
}

fn ground_size<T: Length>(T x) -> int {
    return x.len();
}

test("ground goals keep their own instances") {
    assert!(ground_size(Vec::from([1, 2, 3])) == 3);
    assert!(ground_size([1, 2]) == 2);
    assert!(ground_size((1, "a")) == 2);
}

test("through a shared body") {
    let v = vec_size;
    assert!(v(Vec::from([1, 2])) == 2);
    let a = array_size;
    assert!(a(1.5) == 4);
    let p = pair_size;
    assert!(p(true, 1) == 5);
    let r = record_size;
    assert!(r("s") == 2);
}
