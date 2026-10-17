// A bounded generic function in another module: callers reach its shared
// body with trait dictionaries (examples/perf/dict_once.hy).

fn larger<T: Ord>(T a, T b) -> T {
    if a > b {
        return a;
    }
    return b;
}
