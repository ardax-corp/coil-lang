//! Every body of the files below (the former `mir_direct_*` corpus, now
//! filed by language domain) lowers HIR -> MIR and takes its stack IL from
//! that MIR: none keeps emit_hir's stack lowering. A shape direct MIR
//! refuses gets a file here (M5p), so the census and the harness see it too.

use std::path::PathBuf;
use std::process::Command;

const FILES: &[&str] = &[
    "tests/codegen/new_field_elision.hy",
    "tests/collections/array_heap_alloc.hy",
    "tests/collections/fixed_array_copy_elems.hy",
    "tests/collections/fixed_array_pair_enums.hy",
    "tests/collections/fixed_array_store_shapes.hy",
    "tests/collections/frame_slot_aggregates.hy",
    "tests/collections/heap_arrays.hy",
    "tests/collections/linalg_kernels.hy",
    "tests/collections/matmul_tuples.hy",
    "tests/collections/vec_literal_slots.hy",
    "tests/collections/vec_pop_boxed_option.hy",
    "tests/collections/vec_push.hy",
    "tests/control_flow/defer_alloc.hy",
    "tests/control_flow/diverging_bodies.hy",
    "tests/data/boxed_enum_call_results.hy",
    "tests/data/boxed_enum_match.hy",
    "tests/data/class_fields.hy",
    "tests/data/enum_variant_make.hy",
    "tests/data/heap_payload_pairs.hy",
    "tests/data/heap_tuple_calls.hy",
    "tests/data/mixed_float_int_payloads.hy",
    "tests/data/pair_heap_payloads.hy",
    "tests/data/scalar_enums_string_bool.hy",
    "tests/data/tuple_pair_boxing.hy",
    "tests/data/two_word_pairs.hy",
    "tests/errors/option_result_eq.hy",
    "tests/functions/closures.hy",
    "tests/functions/fn_value_adapters.hy",
    "tests/functions/lambda_bodies.hy",
    "tests/functions/lambda_queue.hy",
    "tests/functions/scalar_calls.hy",
    "tests/generics/generic_class_fields.hy",
    "tests/generics/generic_open_field_slots.hy",
    "tests/generics/generic_vec_stores.hy",
    "tests/generics/mono_nested_params.hy",
    "tests/generics/pair_enum_generic_args.hy",
    "tests/generics/shared_bound_values.hy",
    "tests/generics/shared_generic_calls.hy",
    "tests/host/host_calls.hy",
    "tests/memory/class_local_escape.hy",
    "tests/memory/niche_array_elems.hy",
    "tests/memory/niche_option_result.hy",
    "tests/memory/packed_bool_byte_elems.hy",
    "tests/modules/statics_and_consts.hy",
    "tests/patterns/match_arm_option_values.hy",
    "tests/patterns/scalar_match.hy",
    "tests/patterns/unresolved_payload_arm.hy",
    "tests/strings/format_calls.hy",
    "tests/strings/show_shapes.hy",
    "tests/strings/string_byte_index.hy",
    "tests/strings/string_concat.hy",
    "tests/strings/string_values.hy",
    "tests/syntax/inc_dec_values.hy",
    "tests/tasks/gen_block_on.hy",
    "tests/tasks/gen_result_ok_wrap.hy",
    "tests/testing/assert_returns_err.hy",
    "tests/traits/generic_instance_clones.hy",
    "tests/traits/raw_instance_args.hy",
    "tests/traits/static_trait_call_results.hy",
    "tests/traits/unit_receiver_trait.hy",
    "tests/traits/user_operators_lowering.hy",
    "tests/types/bool_casts.hy",
    "tests/types/byte_values.hy",
];

#[test]
fn mir_direct_positive_tests_lower_every_body() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let files: Vec<PathBuf> = FILES.iter().map(|f| root.join(f)).collect();
    let out_dir = std::env::temp_dir().join(format!("coil_direct_mir_{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let mut refused = Vec::new();
    for file in &files {
        let mut cmd = Command::new(&bin);
        cmd.args(["compile", "--opt-stats", "--include-tests"]);
        cmd.arg("--root").arg(root.join("examples/src"));
        for extra in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(extra);
        }
        let out = cmd
            .args(["--allow-read", "--allow-write", "--allow-net", "--allow-env"])
            .arg(file)
            .arg("-o")
            .arg(out_dir.join("t.hyc"))
            .output()
            .expect("coil compile --opt-stats");
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{}: compile failed: {text}", file.display());
        // `    name: tier (dense: …; lir: …; hir: …)`, one per body.
        for line in text.lines().filter(|l| l.starts_with("    ") && l.contains("; hir: ")) {
            if !line.contains("; hir: lowered)") || line.contains(": fuse (") {
                refused.push(format!("{}: {}", file.display(), line.trim()));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    assert!(refused.is_empty(), "bodies left on the HIR stack lowering:\n{}", refused.join("\n"));
}
