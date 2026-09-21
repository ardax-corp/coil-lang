// COI-404: cross-module Result<Path, IoError> (callee registers IoError).
use io::{IoError};

class Path {
    pub raw: string,
}

impl Path {
    pub static fn from(string s) -> Path {
        return new Path(s);
    }

    pub fn as_str() -> string {
        return self.raw;
    }

    pub fn clone_ok() -> Result<Path, IoError> {
        return new Path(self.raw);
    }

    pub fn join_empty(Path other) -> Result<Path, IoError> {
        if len(other.raw) == 0 {
            return new Path(self.raw);
        }
        return new Path(self.raw);
    }
}
