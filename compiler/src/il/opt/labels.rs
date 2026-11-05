//! Label id space of an op buffer: the next free id, and remapping a
//! chunk's labels into a fresh range.

use std::collections::HashMap;

use super::super::op::{IlOp, Label};
