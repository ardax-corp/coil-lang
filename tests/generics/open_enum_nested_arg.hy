// An open `Option` / `Result` (its payload still a type parameter) passed
// to another generic inside a type argument, `Vec<U>` at `U =
// Option<T>` (coil-lang#2138): both shared bodies agree on the element.
use tests::assert;
use task::{Channel, channel, TaskError};

fn one<U>(U x) -> Vec<U> {
    let v: Vec<U> = Vec::new();
    v.push(x);
    return v;
}

fn empty<U>(int n) -> Vec<U> {
    let v: Vec<U> = Vec::new();
    return v;
}

fn some_in_vec<T>(T x) -> Vec<Option<T>> {
    let r: Option<T> = Option::Some(x);
    let v = one(r);
    v.push(Option::None);
    return v;
}

fn ok_in_vec<T>(T x) -> Vec<Result<T, string>> {
    let c: Vec<Result<T, string>> = empty(4);
    c.push(Result::Ok(x));
    c.push(Result::Err("no"));
    return c;
}

fn first_some<T>(Vec<Option<T>> v) -> Option<T> {
    let w = one(v[0]);
    return w[0];
}

fn result_channel<T>() -> Channel<Result<T, TaskError>> {
    let c: Channel<Result<T, TaskError>> = channel(4);
    return c;
}

test("an open Option inside a Vec type argument") {
    let v = some_in_vec(5);
    assert!(v.len() == 2);
    assert!(v[0] == Option::Some(5));
    assert!(v[1] == Option::None);
    let s = some_in_vec("a");
    assert!(s[0] == Option::Some("a"));
    assert!(first_some(v) == Option::Some(5));
}

test("an open Result inside a Vec type argument") {
    let v = ok_in_vec(7);
    assert!(v[0] == Result::Ok(7));
    assert!(v[1] == Result::Err("no"));
    let w = ok_in_vec("x");
    assert!(w[0] == Result::Ok("x"));
}

test("a channel of an open Result") {
    let c: Channel<Result<int, TaskError>> = result_channel();
    let _ = c.try_send(Result::Ok(3));
    let _ = c.try_send(Result::Err(TaskError::TimedOut));
    assert!(c.try_recv() == Result::Ok(Result::Ok(3)));
    let second = match c.try_recv() {
        Result::Ok(Result::Err(TaskError::TimedOut)) => true,
        default => false,
    };
    assert!(second);
}

fn none_in_vec<T>(T x) -> Vec<Option<T>> {
    let r: Option<T> = Option::None;
    let v = one(r);
    v.push(Option::Some(x));
    return v;
}

enum Shape {
    Dot,
    Box(int),
}

fn wrap_any<U>(U x) -> Vec<U> {
    return one(x);
}

test("None and a ground heap enum pass through a shared body") {
    let v = none_in_vec(4);
    assert!(v[0] == Option::None);
    assert!(v[1] == Option::Some(4));
    let shapes = wrap_any(Shape::Box(3));
    let ok = match shapes[0] {
        Shape::Box(n) => n == 3,
        Shape::Dot => false,
    };
    assert!(ok);
    let dots = wrap_any(Shape::Dot);
    let dot = match dots[0] {
        Shape::Dot => true,
        default => false,
    };
    assert!(dot);
}
