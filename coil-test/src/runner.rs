//! Discovery, per-file compile, and per-case execution.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;

use coil_host::{ExecutePipelineArgs, bind_cli_roots, execute_pipeline, wire_pipeline_vm};
use common::Byte;
use compiler::{HostGrants, OptLevel, Pipeline};
use machine::Machine;
use reporting::{ErrorCode, ReportConfig, ReportFormat};

/// What to run and how to compile it.
#[derive(Debug, Clone)]
pub struct TestOptions {
    /// Test root (a directory walked for `.hy` files).
    pub root: PathBuf,
    /// Stop after the first failed case.
    pub fail_fast: bool,
    pub opt_level: OptLevel,
    pub grants: HostGrants,
    /// Extra `--root` module search directories.
    pub extra_roots: Vec<PathBuf>,
}

fn writer_for(format: ReportFormat) -> Box<dyn Write + Send> {
    match format {
        ReportFormat::Pretty => Box::new(std::io::stderr()),
        ReportFormat::Sarif | ReportFormat::Lsp => Box::new(std::io::stdout()),
    }
}

/// Every `.hy` file under `dir`, recursively, sorted by full path.
pub fn collect_test_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("tests directory `{}` not found", dir.display()));
    }
    let mut files = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("unable to read `{}`: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("unable to read directory entry: {e}"))?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("hy") {
                out.push(path);
            }
        }
        Ok(())
    }
    walk(dir, &mut files)?;
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "no `.hy` test files found under `{}`",
            dir.display()
        ));
    }
    Ok(files)
}

/// Negative syntax / type tests live under any path segment named `compile_fail`.
/// Those files must fail to compile; a successful compile is a harness failure.
pub fn is_compile_fail(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "compile_fail")
}

/// Classify a `catch_unwind` compile result for a `compile_fail/` file.
/// Only a clean diagnostic rejection (`Ok(Err(_))`) is harness success.
/// Panic does not count (release builds use `panic = "abort"`).
fn compile_fail_rejected<T, E>(compiled: &std::thread::Result<Result<T, E>>) -> bool {
    matches!(compiled, Ok(Err(_)))
}

/// Run one `test("…")` case on a fresh VM (static init first). `true` = passed.
pub fn run_test_case(
    pipeline: &Pipeline,
    bytecode: &[Byte],
    constants: &[u64],
    strings: &[String],
    entry: Option<&Path>,
    name: &str,
    offset: u32,
) -> bool {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut machine = Machine::<256>::default();
        wire_pipeline_vm(pipeline, &mut machine, entry);
        machine.set_program_debug(pipeline.program_debug());
        machine.init_static_slots(pipeline.static_slot_count());
        machine.load_program(bytecode, constants, strings);
        if let Some(main) = pipeline.main_offset() {
            let setup = pipeline.prologue_jmp_target();
            if setup != main {
                machine.halt_first_jump_to(setup as usize, main);
                machine.run_from(setup as usize);
            }
        }
        let ret = machine.call_function(offset, &[]);
        let ok = !machine.panicked() && machine.result_is_ok(ret);
        let reason = if ok || machine.panicked() {
            None
        } else {
            machine.result_err_text(ret)
        };
        (ok, reason)
    }));
    match result {
        Ok((ok, reason)) => {
            if !ok {
                match reason {
                    // `assert(cond, "message")?` returns `Err("message")`.
                    Some(reason) => eprintln!("> Test \"{name}\" failed: {reason}"),
                    None => eprintln!("> Test \"{name}\" failed"),
                }
            }
            ok
        }
        Err(_) => {
            eprintln!("> Test \"{name}\" failed");
            false
        }
    }
}

/// Run the harness over `options.root` and return `(passed, failed)` without exiting.
pub fn run_test_suite(
    config: ReportConfig,
    options: &TestOptions,
) -> Result<(usize, usize), String> {
    let files = collect_test_files(&options.root)?;

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut stop = false;

    for path in &files {
        if stop {
            break;
        }
        let display = path.display().to_string();
        let expect_compile_fail = is_compile_fail(path);
        let format = config.format;
        // Expected compile rejection: suppress ariadne noise so the harness
        // summary stays readable when many compile_fail files exist.
        let mut pipeline = if expect_compile_fail {
            Pipeline::with_reporter(config.clone(), Box::new(std::io::sink()))
        } else {
            Pipeline::with_reporter(config.clone(), writer_for(format))
        };
        pipeline.set_include_tests(true);
        pipeline.set_opt_level(options.opt_level);
        pipeline.set_host_grants(options.grants.clone());
        // Same search path CI passes with `--root`: examples and a sibling
        // coil-stdlib checkout, when those directories exist.
        let mut roots = options.extra_roots.clone();
        for extra in compiler::Pipeline::workspace_language_extra_roots() {
            if extra.is_dir() && !roots.contains(&extra) {
                roots.push(extra);
            }
        }
        bind_cli_roots(&mut pipeline, roots);

        // catch_unwind isolates a compiler ICE from aborting the whole
        // harness under panic=unwind. Release builds use panic=abort, so
        // compile_fail fixtures must reject via Ok(Err(_)), not panic.
        let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline.compile_src_from_file(&display)
        }));
        let cases: Vec<(String, u32)> = pipeline.test_cases().to_vec();
        let _ = pipeline.finish_reporting();

        let file_ok = if expect_compile_fail {
            // Only a clean diagnostic rejection counts. A panic is a
            // harness failure (and aborts under release panic=abort).
            if compile_fail_rejected(&compiled) {
                passed += 1;
                true
            } else {
                failed += 1;
                match &compiled {
                    Ok(Ok(_)) => {
                        eprintln!("> Test \"{display}\" failed (expected compile failure)");
                    }
                    Err(_) => {
                        eprintln!("> Test \"{display}\" failed (compiler panicked)");
                    }
                    Ok(Err(_)) => unreachable!("compile_fail_rejected is true for Ok(Err(_))"),
                }
                if options.fail_fast {
                    stop = true;
                }
                false
            }
        } else {
            match compiled {
                Err(_) => {
                    failed += 1;
                    eprintln!("> Test \"{display}\" failed (compiler panicked)");
                    if options.fail_fast {
                        stop = true;
                    }
                    false
                }
                Ok(Err(_)) => {
                    failed += 1;
                    eprintln!("> Test \"{display}\" failed");
                    if options.fail_fast {
                        stop = true;
                    }
                    false
                }
                Ok(Ok((bytecode, constants))) => {
                    let strings = pipeline.strings().to_vec();
                    let static_slots = pipeline.static_slot_count();
                    let entry = path.as_path();
                    if cases.is_empty() {
                        // Legacy: whole-file `main` is one opaque case.
                        let debug = pipeline.program_debug();
                        let operand_stack_slots = pipeline.operand_stack_slots();
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            execute_pipeline(ExecutePipelineArgs {
                                pipeline: &pipeline,
                                bytecode: &bytecode,
                                constants: &constants,
                                strings: &strings,
                                static_slots,
                                debug,
                                entry: Some(entry),
                                operand_stack_slots,
                            })
                        }));
                        let ok = match result {
                            Ok(panicked) => !panicked,
                            Err(_) => false,
                        };
                        if ok {
                            passed += 1;
                        } else {
                            failed += 1;
                            eprintln!("> Test \"{display}\" failed");
                            if options.fail_fast {
                                stop = true;
                            }
                        }
                        ok
                    } else {
                        let mut any_fail = false;
                        for (name, offset) in &cases {
                            let ok = run_test_case(
                                &pipeline,
                                &bytecode,
                                &constants,
                                &strings,
                                Some(entry),
                                name,
                                *offset,
                            );
                            if ok {
                                passed += 1;
                            } else {
                                failed += 1;
                                any_fail = true;
                                if options.fail_fast {
                                    stop = true;
                                    break;
                                }
                            }
                        }
                        !any_fail
                    }
                }
            }
        };

        if file_ok {
            eprintln!("ok   {display}");
        } else {
            eprintln!("FAILED {display}");
        }
    }

    Ok((passed, failed))
}

/// `coil test` entry: run the suite, print the summary, exit non-zero on failure.
pub fn cmd_test(config: ReportConfig, options: TestOptions) {
    let (passed, failed) = match run_test_suite(config.clone(), &options) {
        Ok(counts) => counts,
        Err(msg) => {
            let format = config.format;
            let mut pipeline = Pipeline::with_reporter(config, writer_for(format));
            pipeline.emit_spanless_error(ErrorCode::IoError, msg);
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };

    eprintln!();
    eprintln!(
        "test result: {}. {passed} passed; {failed} failed; {} total",
        if failed == 0 { "ok" } else { "FAILED" },
        passed + failed
    );

    if failed != 0 {
        exit(1);
    }
}

#[cfg(test)]
#[path = "runner.tests.rs"]
mod tests;
