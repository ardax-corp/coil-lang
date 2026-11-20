//! Every body of the files below (the former `mir_direct_*` corpus, now
//! filed by language domain) lowers HIR -> MIR and is emitted from
//! that MIR: none keeps emit_hir's stack lowering. A shape direct MIR
//! refuses gets a file here (M5p), so the census and the harness see it too.

use std::path::PathBuf;
use std::process::Command;

const FILES: &[&str] = &[
    "tests/codegen/new_field_elision.coil",
    "tests/collections/array_heap_alloc.coil",
    "tests/collections/fixed_array_copy_elems.coil",
    "tests/collections/fixed_array_pair_enums.coil",
    "tests/collections/fixed_array_store_shapes.coil",
    "tests/collections/frame_slot_aggregates.coil",
    "tests/collections/heap_arrays.coil",
    "tests/collections/linalg_kernels.coil",
    "tests/collections/matmul_tuples.coil",
    "tests/collections/vec_literal_slots.coil",
    "tests/collections/vec_pop_boxed_option.coil",
    "tests/collections/vec_push.coil",
    "tests/control_flow/defer_alloc.coil",
    "tests/control_flow/diverging_bodies.coil",
    "tests/data/boxed_enum_call_results.coil",
    "tests/data/boxed_enum_match.coil",
    "tests/data/class_fields.coil",
    "tests/data/enum_variant_make.coil",
    "tests/data/heap_payload_pairs.coil",
    "tests/data/heap_tuple_calls.coil",
    "tests/data/mixed_float_int_payloads.coil",
    "tests/data/pair_heap_payloads.coil",
    "tests/data/scalar_enums_string_bool.coil",
    "tests/data/tuple_pair_boxing.coil",
    "tests/data/two_word_pairs.coil",
    "tests/errors/option_result_eq.coil",
    "tests/functions/closures.coil",
    "tests/functions/fn_value_adapters.coil",
    "tests/functions/lambda_bodies.coil",
    "tests/functions/lambda_queue.coil",
    "tests/functions/scalar_calls.coil",
    "tests/generics/generic_class_fields.coil",
    "tests/generics/generic_open_field_slots.coil",
    "tests/generics/generic_vec_stores.coil",
    "tests/generics/mono_nested_params.coil",
    "tests/generics/pair_enum_generic_args.coil",
    "tests/generics/shared_bound_values.coil",
    "tests/generics/shared_generic_calls.coil",
    "tests/host/host_calls.coil",
    "tests/memory/class_local_escape.coil",
    "tests/memory/niche_array_elems.coil",
    "tests/memory/niche_option_result.coil",
    "tests/memory/packed_bool_byte_elems.coil",
    "tests/modules/statics_and_consts.coil",
    "tests/patterns/match_arm_option_values.coil",
    "tests/patterns/scalar_match.coil",
    "tests/patterns/unresolved_payload_arm.coil",
    "tests/strings/format_calls.coil",
    "tests/strings/show_shapes.coil",
    "tests/strings/string_byte_index.coil",
    "tests/strings/string_concat.coil",
    "tests/strings/string_values.coil",
    "tests/syntax/inc_dec_values.coil",
    "tests/tasks/gen_block_on.coil",
    "tests/tasks/gen_result_ok_wrap.coil",
    "tests/testing/assert_returns_err.coil",
    "tests/traits/generic_instance_clones.coil",
    "tests/traits/raw_instance_args.coil",
    "tests/traits/static_trait_call_results.coil",
    "tests/traits/unit_receiver_trait.coil",
    "tests/traits/user_operators_lowering.coil",
    "tests/types/bool_casts.coil",
    "tests/types/byte_values.coil",
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
            .arg(out_dir.join("t.ccoil"))
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
