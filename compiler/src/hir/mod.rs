//! High-level IR (HIR): a typed, desugared tree built after `check_program`.
//!
//! The port is phased (see the HIR plan doc). Phase 0 is [`layout`]: the one
//! query that decides how a value of a given type is represented, so the
//! AST codegen, MIR and the future HIR lowering cannot disagree.

pub mod layout;
