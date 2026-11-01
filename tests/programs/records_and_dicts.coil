// Anonymous records: construction, field reads, structural equality and a `Vec` of records folded into a total.
// @output alice 31 { age: 31, name: alice } true [{ k: a, v: 1 }, { k: b, v: 2 }] 3\n
use io::sync::println;
use string::format;

fn main() {
    let p = { name: "alice", age: 30 };
    let q = { name: p.name, age: p.age + 1 };
    let same = q == { name: "alice", age: 31 };
    let ks = Vec::from(["a", "b"]);
    let vs = Vec::from([1, 2]);
    let es = Vec::new();
    let i = 0;
    while i < ks.len() {
        es.push({ k: ks[i], v: vs[i] });
        i += 1;
    }
    let tot = 0;
    for e in es {
        tot += e.v;
    }
    let _ = println(format("%s %i %v %z %v %i", q.name, q.age, q, same, es, tot));
}
