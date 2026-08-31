//! PGO unit tests (COI-132).

use super::*;
use crate::il::{IlJumpKind, IlOp, Label};
use common::DebugLoc;

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

fn jmpf(id: u32) -> IlOp {
    IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: Label(id),
        loc: loc(),
        hint: Default::default(),
    }
}

fn jmp(id: u32) -> IlOp {
    IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: Label(id),
        loc: loc(),
        hint: Default::default(),
    }
}

fn label(id: u32) -> IlOp {
    IlOp::Label(Label(id))
}

fn ret() -> IlOp {
    IlOp::Return { loc: loc() }
}

fn c(n: i32) -> IlOp {
    IlOp::Const {
        imm: n,
        loc: loc(),
    }
}

/// `JMPF 1; CONST 0; RETURN; L1: CONST 1; RETURN`
fn terminating_then() -> Vec<IlOp> {
    vec![c(1), jmpf(1), c(0), ret(), label(1), c(1), ret()]
}

#[test]
fn instrument_records_function_block_and_branch_sites() {
    let ops = terminating_then();
    let map = instrument_for_pgo(&ops);
    assert_eq!(map.functions.len(), 1);
    assert!(map.blocks.len() >= 2, "then-arm and join are separate blocks");
    assert_eq!(map.branches.len(), 1);
    assert!(matches!(
        ops[map.branches[0]],
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            ..
        }
    ));
}

#[test]
fn instrument_inserts_hostinvoke_counters() {
    let mut ops = terminating_then();
    let before = ops.len();
    let _ = instrument_for_pgo_mut(&mut ops);
    assert!(ops.len() > before);
    assert!(ops.iter().any(|op| matches!(op, IlOp::HostInvoke { .. })));
}

#[test]
fn profile_round_trip_json_and_hits() {
    let mut p = ProfileData::new();
    p.hit_function("main");
    p.hit_function("main");
    p.hit_block(0);
    p.hit_branch(0, true);
    p.hit_branch(0, false);
    let json = p.to_json();
    let q = ProfileData::from_json(&json).expect("parse");
    assert_eq!(q.function_counts.get("main"), Some(&2));
    assert_eq!(q.block_counts.get(&0), Some(&1));
    assert_eq!(q.branch_counts.get(&0), Some(&(1, 1)));
    assert!(json.contains("\"version\":2"));
}

#[test]
fn prepare_ignores_fn_on_checksum_mismatch() {
    begin_pgo_module();
    next_pgo_function("main");
    let ops = terminating_then();
    let map = heat::instrument_map_for(&ops, "main");
    let good = fn_shape_checksum(&ops, &map, "main");
    let mut profile = ProfileData::new();
    profile.fn_checksums.insert("main".into(), good ^ 1);
    profile.block_counts.insert(0, 99);
    profile.branch_counts.insert(0, (1, 100));
    set_current_profile(Some(profile));
    prepare_function_profile(&ops);
    assert!(fn_profile_ignored());
    assert_eq!(block_heat_current(&ops, 0), 0);
    let bp = branch_profile(&ops, &current_profile().unwrap());
    assert!(bp.taken.is_empty() && bp.not_taken.is_empty());
    set_current_profile(None);
    begin_pgo_module();
}

#[test]
fn prepare_caches_map_when_checksum_matches() {
    begin_pgo_module();
    next_pgo_function("main");
    let ops = terminating_then();
    let map = heat::instrument_map_for(&ops, "main");
    let good = fn_shape_checksum(&ops, &map, "main");
    let mut profile = ProfileData::new();
    profile.fn_checksums.insert("main".into(), good);
    profile.block_counts.insert(0, 42);
    set_current_profile(Some(profile));
    prepare_function_profile(&ops);
    assert!(!fn_profile_ignored());
    assert!(cached_instrument_map().is_some());
    assert_eq!(block_heat_current(&ops, 0), 42);
    set_current_profile(None);
    begin_pgo_module();
}

#[test]
fn instrument_path_skips_branch_layout() {
    use crate::il::opt::{optimize, OptimizeOptions};
    set_pgo_instrument(true);
    begin_pgo_module();
    next_pgo_function("main");
    let mut ops = terminating_then();
    optimize(&mut ops, &OptimizeOptions::default(), &mut Vec::new());
    let inverted = ops.iter().any(|op| {
        matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                ..
            }
        )
    });
    assert!(
        !inverted,
        "pgo-instrument must not run layout after cleanup"
    );
    set_pgo_instrument(false);
    begin_pgo_module();
}

#[test]
fn instrument_dump_use_profile_layout_smoke() {
    use crate::il::opt::{optimize, OptimizeOptions};

    // Phase A: instrument compile records checksums on cleanup IR.
    set_pgo_instrument(true);
    begin_pgo_module();
    next_pgo_function("main");
    let mut inst = terminating_then();
    optimize(&mut inst, &OptimizeOptions::default(), &mut Vec::new());
    let map = instrument_for_pgo(&inst);
    let cs = fn_shape_checksum(&inst, &map, "main");
    let mut keys = std::collections::BTreeMap::new();
    keys.insert(0, 20);
    let mut blocks = std::collections::BTreeMap::new();
    blocks.insert(0, 20);
    let mut branches = std::collections::BTreeMap::new();
    // Hot fall-through (not-taken).
    branches.insert(0, (1, 100));
    let mut profile = profile_from_runtime(&keys, blocks, branches);
    profile.fn_checksums.insert("main".into(), cs);
    set_pgo_instrument(false);

    // Phase B: use-profile keeps hot fall-through (no invert).
    begin_pgo_module();
    next_pgo_function("main");
    set_current_profile(Some(profile));
    let mut use_ops = terminating_then();
    optimize(&mut use_ops, &OptimizeOptions::default(), &mut Vec::new());
    let inverted = use_ops.iter().any(|op| {
        matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                ..
            }
        )
    });
    assert!(!inverted, "hot fall-through must not invert under use-profile");

    // Without profile, heuristic still inverts.
    set_current_profile(None);
    begin_pgo_module();
    next_pgo_function("main");
    let mut cold = terminating_then();
    optimize(&mut cold, &OptimizeOptions::default(), &mut Vec::new());
    let inverted_cold = cold.iter().any(|op| {
        matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                ..
            }
        )
    });
    assert!(inverted_cold, "no profile → layout may invert");
    begin_pgo_module();
}

#[test]
fn profile_binary_round_trip_and_optional_timestamp() {
    let mut p = ProfileData::new();
    p.hit_function("main");
    assert!(p.timestamp > 0);
    let bytes = p.to_binary().expect("encode");
    let q = ProfileData::from_binary(&bytes).expect("decode");
    assert_eq!(q.function_counts.get("main"), Some(&1));
    assert_eq!(q.timestamp, p.timestamp);
    let old = "{\"version\":1,\"function_counts\":{},\"block_counts\":{},\"branch_counts\":{}}";
    let r = ProfileData::from_json(old).expect("legacy json");
    assert_eq!(r.timestamp, 0);
}

#[test]
fn profile_json_and_binary_files_round_trip() {
    let dir = std::env::temp_dir().join(format!("coil-pgo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let json_path = dir.join("p.json");
    let bin_path = dir.join("p.bin");
    let mut p = ProfileData::new();
    p.hit_function("main");
    p.to_json_file(&json_path).unwrap();
    p.to_binary_file(&bin_path).unwrap();
    let j = ProfileData::from_json_file(&json_path).unwrap();
    let b = ProfileData::from_binary_file(&bin_path).unwrap();
    assert_eq!(j.function_counts.get("main"), Some(&1));
    assert_eq!(b.function_counts.get("main"), Some(&1));
    let missing = dir.join("nope.json");
    assert!(matches!(
        ProfileData::from_json_file(&missing),
        Err(LoadError::Io(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_rejects_wrong_version_and_empty() {
    match ProfileData::from_json("{\"version\":99}") {
        Err(LoadError::Version { found: 99, expected: PROFILE_VERSION }) => {}
        other => panic!("{other:?}"),
    }
    assert!(matches!(ProfileData::from_json(""), Err(LoadError::Parse(_))));
}

#[test]
fn missing_profile_is_not_cold() {
    let p = ProfileData::new();
    assert!(!p.function_is_cold("main"));
    assert!(!p.function_is_hot("main"));
}

#[test]
fn optimize_with_profile_keeps_hot_fallthrough() {
    begin_pgo_module();
    let mut cold = terminating_then();
    optimize_with_profile(&mut cold, &ProfileData::new());
    let inverted_without = cold.iter().any(|op| {
        matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                ..
            }
        )
    });

    let mut hot_ft = terminating_then();
    let mut profile = ProfileData::new();
    profile.branch_counts.insert(0, (1, 100));
    optimize_with_profile(&mut hot_ft, &profile);
    let inverted_hot = hot_ft.iter().any(|op| {
        matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                ..
            }
        )
    });
    assert!(inverted_without, "heuristic still inverts a cold then-arm");
    assert!(
        !inverted_hot,
        "hot not-taken (fall-through) must not invert"
    );
}

#[test]
fn current_profile_guides_hot_cold() {
    let mut p = ProfileData::new();
    p.function_counts.insert("hot".into(), 20);
    p.function_counts.insert("cold".into(), 0);
    set_current_profile(Some(p));
    assert!(current_function_is_hot("hot"));
    assert!(current_function_is_cold("cold"));
    set_current_profile(None);
    assert!(!current_function_is_hot("hot"));
}

#[test]
fn jmp_only_buffer_instruments_without_branches() {
    let ops = vec![jmp(1), label(1), ret()];
    let map = instrument_for_pgo(&ops);
    assert!(map.branches.is_empty());
    assert!(!map.blocks.is_empty());
}

#[test]
fn profile_from_runtime_maps_function_keys() {
    begin_pgo_module();
    next_pgo_function("main");
    let mut keys = std::collections::BTreeMap::new();
    keys.insert(0, 9);
    let p = profile_from_runtime(&keys, Default::default(), Default::default());
    assert_eq!(p.function_counts.get("main"), Some(&9));
    begin_pgo_module();
}
