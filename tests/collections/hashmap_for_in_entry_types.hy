// @output err\nok\nerr\n3\n
// `for e in m` over a `HashMap<K, V>` types `e` as `Entry<K, V>` at the
// map's own `K` and `V`, so `e.value` is a `V` (#1912). Before, the entry's
// type was open: `return e.value` in a function returning `Result<(), int>`
// read it as the `Ok` payload, a compile error for `()` and a silent `Ok`
// for `Result<bool, int>`.
use collections::map::HashMap;
use io::sync::println;

fn unit_entry(Result<(), int> x) -> Result<(), int> {
    let m: HashMap<string, Result<(), int>> = HashMap::new();
    m.insert("k", x);
    for e in m {
        return e.value;
    }
    return x;
}

fn bool_entry(Result<bool, int> x) -> Result<bool, int> {
    let m: HashMap<string, Result<bool, int>> = HashMap::new();
    m.insert("k", x);
    for e in m {
        return e.value;
    }
    return x;
}

fn say<T>(Result<T, int> r) -> string {
    return match r {
        Ok(_) => "ok",
        Err(_) => "err",
    };
}

fn main() {
    println(say(unit_entry(Err(3))))?;
    println(say(unit_entry(Ok(()))))?;
    println(say(bool_entry(Err(4))))?;
    let m: HashMap<string, int> = HashMap::new();
    m.insert("a", 1);
    m.insert("b", 2);
    let total = 0;
    for e in m {
        total += e.value;
    }
    println(show(total))?;
}
