// Module-level shapes from the library repos that HIR lowering must keep
// (see tests/positive/hir_lib_shapes.hy).

#[derive(Eq, Ord)]
enum Level {
    Debug = 0,
    Info = 1,
    Warn = 2,
}

class Gate {
    min: Level,
}

impl Gate {
    pub static fn new(Level min) -> Gate {
        return new Gate(min);
    }

    pub fn pass(Level level) -> bool {
        return level >= self.min;
    }

    pub fn below(Level level) -> bool {
        return level < self.min;
    }
}

enum ShapeError {
    Invalid { message: string },
}

enum Raw {
    Good { value: int },
    Bad { code: int },
}

fn invalid(string message) -> ShapeError {
    return ShapeError::Invalid{ message: message };
}

fn decode(Raw raw) -> Result<int, ShapeError> {
    return match raw {
        Raw::Good{ value } => value,
        Raw::Bad{ code } => match code {
            1 => raise invalid("one"),
            default => raise invalid("other"),
        },
    };
}

fn message(ShapeError e) -> string {
    return match e {
        ShapeError::Invalid{ message } => message,
    };
}
