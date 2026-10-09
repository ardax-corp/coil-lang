// Typed inlining splices small scalar functions from other modules
// (`ascii`, `num`): their calls inside are named by key, so the next
// round inlines them too.

use ascii::is_alnum;
use ascii::is_space;
use num::abs;
use num::rem_euclid;

fn words(string s) -> int {
    let n = 0;
    let i = 0;
    let inside = false;
    while i < len(s) {
        let c = s[i];
        if is_alnum(c) {
            if !inside {
                n = n + 1;
            }
            inside = true;
        } else if is_space(c) {
            inside = false;
        }
        i = i + 1;
    }
    return n;
}

test("cross-module byte classifiers") {
    assert(words("one two  three") == 3)?;
    assert(words("  a1 _ b2") == 2)?;
    assert(is_alnum("z"))?;
    assert(!is_alnum("-"))?;
}

test("cross-module overloads and nested calls") {
    assert(abs(0 - 7) == 7)?;
    assert(abs(0.0 - 2.5) == 2.5)?;
    assert(rem_euclid(0 - 7, 3) == 2)?;
    assert(rem_euclid(7, 0 - 3) == 1)?;
}
