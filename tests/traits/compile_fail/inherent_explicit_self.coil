// @expected E0108 — an inherent method's receiver is implicit; a `Node self`
// parameter would be a second argument shadowing it.
class Node {
    pub tag: int,
}

impl Node {
    pub fn down(Node self, int n) -> int {
        if n == 0 {
            return self.tag;
        }
        return self.down(n - 1) + 1;
    }
}

fn main() {
    let c = new Node(9).down(2);
}
