// Trait declared in its own module, implemented by importers
// (tests/positive/trait_impl_imported.hy, #522).

class Node {
    pub i: int,
    pub s: string,
}

enum DecodeError {
    Missing { key: string },
}

trait FromNode<T> {
    fn from_node(T proto, Node n) -> Result<T, DecodeError> {}

    fn port_or(T proto, Node n, int d) -> int {
        return match proto.from_node(n) {
            Result::Ok(_) => n.i,
            Result::Err(_) => d,
        };
    }
}
