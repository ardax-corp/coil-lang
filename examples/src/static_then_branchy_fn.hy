// Helper module for tests/modules/module_static_before_branchy_fn.hy (#958):
// statics followed by a function whose body has branches, so its labels can
// share ids with the static-init setup that is laid out right after it.

static const RATIO = 3.141592653589793;

static let COUNT: int = 41;

static let NAME: string = "static";

fn magnitude(int x) -> int {
    if x < 0 {
        return 0 - x;
    }
    return x;
}

// A counted loop the compiler may split into parallel loop bodies, laid out
// between this module's code and the static-init setup.
fn power(int base, int exp) -> int {
    if exp < 0 {
        return 0;
    }
    let r = 1;
    let i = 0;
    while i < exp {
        r = r * base;
        i = i + 1;
    }
    return r;
}
