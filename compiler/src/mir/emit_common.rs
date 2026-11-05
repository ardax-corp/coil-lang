//! Helpers shared by the dense (`emit.rs`) and LIR (`emit_lir.rs`)
//! emitters, the dense call convoy plan and saxpy packs. One copy here,
//! so a fix lands in every tier. Tier-specific pieces (register moves,
//! the `Br` condition, last-arm unpack) stay in each emitter.

use std::collections::{BTreeSet, HashSet};

use common::{DebugLoc, Instruction};

use crate::il::IlOp;
use super::func::MirFunc;
use super::inst::{BlockId, MirBinOp, MirCmpOp, MirConst, MirInst, Terminator, ValueId};
use super::ty::MirTy;
