// #1324: an inherent method calls itself through `self`, with the receiver
// implicit, as a free function recurses.
use tests::assert;

class Node {
    pub tag: int,
}

impl Node {
    pub fn down(int n) -> int {
        if n == 0 {
            return self.tag;
        }
        return self.down(n - 1) + 1;
    }
}

test("a method recursing through self") {
    assert!(new Node(9).down(2) == 11);
    assert!(new Node(1).down(0) == 1);
}
