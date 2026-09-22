// Iterative sibling of binary_trees.hy. Same checksum 135854.
// A perfect tree of depth d has 2^(d+1)-1 nodes and item_check sums 1
// per node. This walks that many additions in a counted loop. It does
// not allocate the nodes the recursive version builds.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn walk(int depth) -> int {
    let nodes = (1 << (depth + 1)) - 1;
    let s = 0;
    let i = 0;
    while i < nodes {
        s = s + 1;
        i = i + 1;
    }
    return s;
}

fn main() {
    let n = 10;
    let sum = walk(n + 1);
    let long_lived = walk(n);
    let depth = 4;
    while depth <= n {
        let iterations = 1 << (n - depth + 4);
        let i = 0;
        let c = 0;
        while i < iterations {
            c = c + walk(depth);
            i = i + 1;
        }
        sum = sum + c;
        depth = depth + 2;
    }
    sum = sum + long_lived;
    write_all(stdout(), to_bytes(format("%i", sum)));
}
