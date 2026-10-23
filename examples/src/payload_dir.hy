// Provider for tests/modules/imported_type_in_enum_payload.hy (#1947).

enum Dir {
    North,
    South,
}

fn is_north(Dir d) -> bool {
    return match d {
        Dir::North => true,
        Dir::South => false,
    };
}
