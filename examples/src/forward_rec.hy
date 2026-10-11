// Helper module for tests/positive/forward_imported_class.hy (#888).

class Rec {
    pub n: int,
}

fn make(int n) -> Rec {
    return new Rec(n);
}
