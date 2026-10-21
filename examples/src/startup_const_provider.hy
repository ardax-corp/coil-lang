// Helper module for tests/modules/aliased_unfolded_const.hy: a const that
// does not fold (a function value), set once at startup.

fn twice(int x) -> int {
    return x * 2;
}

const TWICE = twice;
