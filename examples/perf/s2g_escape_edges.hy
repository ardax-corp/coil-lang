// S2g: slot SROA in the private region, one MakeArray at a named escape.
// fill(n) last writes n-3, n-2, n-1 when n % 3 == 0; sum = 3n - 6.

class Holder {
    pub a: [int; 3]
}

fn sum3([int; 3] xs) -> int {
    return xs[0] + xs[1] + xs[2];
}

fn fill_return(int n) -> [int; 3] {
    let xs = [0, 0, 0];
    let i = 0;
    while i < n {
        xs[i % 3] = i;
        i = i + 1;
    }
    return xs;
}

fn pack_call(int n) -> int {
    let xs = [0, 0, 0];
    let i = 0;
    while i < n {
        xs[i % 3] = i;
        i = i + 1;
    }
    return sum3(xs);
}

fn pack_push(int n) -> int {
    let xs = [0, 0, 0];
    let i = 0;
    while i < n {
        xs[i % 3] = i;
        i = i + 1;
    }
    let v = Vec::from([[0, 0, 0]]);
    v.push(xs);
    return sum3(v[1]);
}

fn pack_field(int n) -> int {
    let xs = [0, 0, 0];
    let i = 0;
    while i < n {
        xs[i % 3] = i;
        i = i + 1;
    }
    let h = new Holder([0, 0, 0]);
    h.a = xs;
    return sum3(h.a);
}

fn pack_host(int n) -> int {
    let xs = [0, 0, 0];
    let i = 0;
    while i < n {
        xs[i % 3] = i;
        i = i + 1;
    }
    let v = Vec::from(xs);
    return v[0] + v[1] + v[2];
}

fn main() {
    let n = 6;
    let a = fill_return(n);
    if a[0] + a[1] + a[2] != 12 {
        panic "s2g return";
    }
    if pack_call(n) != 12 {
        panic "s2g call-arg";
    }
    if pack_push(n) != 12 {
        panic "s2g ArrayPush";
    }
    if pack_field(n) != 12 {
        panic "s2g field";
    }
    if pack_host(n) != 12 {
        panic "s2g host";
    }
}
