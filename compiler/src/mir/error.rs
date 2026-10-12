//! Why a MIR emitter refused a body.

use super::builder::MirError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerError {
    Refused(String),
    Mir(MirError),
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(s) => write!(f, "numeric MIR refused: {s}"),
            Self::Mir(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LowerError {}

impl From<MirError> for LowerError {
    fn from(e: MirError) -> Self {
        Self::Mir(e)
    }
}
