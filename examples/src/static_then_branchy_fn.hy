// Helper module for tests/positive/module_static_before_branchy_fn.hy (#958):
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
