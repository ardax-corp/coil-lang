// Provider for tests/modules/imported_type_in_enum_payload.hy: an enum
// whose payloads name a type imported from `payload_dir` (#1947).

use payload_dir::Dir;

enum Command {
    Go(Dir),
    Face { dir: Dir, steps: int },
    Stay,
}

fn go_north() -> Command {
    return Command::Go(Dir::North);
}

fn face_south(int steps) -> Command {
    return Command::Face { dir: Dir::South, steps: steps };
}
