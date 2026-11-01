// Generic free functions over `Vec<T>` for the tests that exercise calls to
// another module's generic function (shared bodies, mono clones, effect
// inference through a higher-order call). The stdlib's own `Vec<T>` API is
// methods (`collections::vec::VecOps`); these keep the free-function shape.

fn map<T, U>(Vec<T> xs, T -> U mapper) -> Vec<U> {
    let out: Vec<U> = Vec::new();
    let i = 0;
    while i < xs.len() {
        out.push(mapper(xs[i]));
        i = i + 1;
    }
    return out;
}

fn filter<T>(Vec<T> xs, T -> bool pred) -> Vec<T> {
    let out: Vec<T> = Vec::new();
    let i = 0;
    while i < xs.len() {
        if pred(xs[i]) {
            out.push(xs[i]);
        }
        i = i + 1;
    }
    return out;
}

fn concat<T>(Vec<T> a, Vec<T> b) -> Vec<T> {
    let out: Vec<T> = Vec::new();
    let i = 0;
    while i < a.len() {
        out.push(a[i]);
        i = i + 1;
    }
    let j = 0;
    while j < b.len() {
        out.push(b[j]);
        j = j + 1;
    }
    return out;
}

fn chunks<T>(Vec<T> xs, int size) -> Vec<Vec<T>> {
    let out: Vec<Vec<T>> = Vec::new();
    if size <= 0 {
        return out;
    }
    let i = 0;
    while i < xs.len() {
        let chunk: Vec<T> = Vec::new();
        let j = 0;
        while j < size && i + j < xs.len() {
            chunk.push(xs[i + j]);
            j = j + 1;
        }
        out.push(chunk);
        i = i + size;
    }
    return out;
}

fn windows<T>(Vec<T> xs, int size) -> Vec<Vec<T>> {
    let out: Vec<Vec<T>> = Vec::new();
    if size <= 0 || size > xs.len() {
        return out;
    }
    let i = 0;
    while i + size <= xs.len() {
        let w: Vec<T> = Vec::new();
        let j = 0;
        while j < size {
            w.push(xs[i + j]);
            j = j + 1;
        }
        out.push(w);
        i = i + 1;
    }
    return out;
}

class Partitioned<T> {
    matched: Vec<T>,
    rest: Vec<T>,
}

impl Partitioned<T> {
    pub fn matched() -> Vec<T> {
        return self.matched;
    }

    pub fn rest() -> Vec<T> {
        return self.rest;
    }
}

fn partition<T>(Vec<T> xs, T -> bool pred) -> Partitioned<T> {
    let matched: Vec<T> = Vec::new();
    let rest: Vec<T> = Vec::new();
    let i = 0;
    while i < xs.len() {
        if pred(xs[i]) {
            matched.push(xs[i]);
        } else {
            rest.push(xs[i]);
        }
        i = i + 1;
    }
    return new Partitioned(matched, rest);
}

fn join(Vec<string> parts, string sep) -> string {
    let out = "";
    let i = 0;
    while i < parts.len() {
        if i > 0 {
            out = out + sep;
        }
        out = out + parts[i];
        i = i + 1;
    }
    return out;
}
