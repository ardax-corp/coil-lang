//! `coil-fmt` — format `.hy` sources (single file or directory).
//!
//! Preserves `//` comments and `///` docs attached to declarations. Default
//! rewrites in place; `--check` exits non-zero when reformatting would change a file.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;

use clap::{CommandFactory, FromArgMatches, Parser};
use parser::format_source;
use reporting::{Message, ReportConfig, SourceMap, create_sink};

#[derive(Parser, Debug)]
#[command(
    name = "coil-fmt",
    about = "Format coil source files",
    disable_help_subcommand = true,
    after_help = "Directories are walked recursively for `*.hy`.\n\
Preserves `//` comments and `///` doc comments on declarations."
)]
struct FmtArgs {
    /// Exit 1 if any file would change (no writes)
    #[arg(long)]
    check: bool,
    /// Files or directories to format
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<Option<FmtArgs>, String> {
    let mut command = FmtArgs::command();
    command.set_bin_name("coil-fmt");
    match command.try_get_matches_from(args) {
        Ok(matches) => FmtArgs::from_arg_matches(&matches)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn collect_hy_files(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if root.is_file() {
        if root.extension().and_then(|e| e.to_str()) == Some("hy") {
            out.push(root.to_path_buf());
        } else {
            return Err(format!("`{}` is not a `.hy` file", root.display()));
        }
        return Ok(());
    }
    if !root.is_dir() {
        return Err(format!("path not found: {}", root.display()));
    }
    let entries = fs::read_dir(root).map_err(|e| format!("read {}: {e}", root.display()))?;
    let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    children.sort();
    for child in children {
        if child.is_dir() {
            collect_hy_files(&child, out)?;
        } else if child.extension().and_then(|e| e.to_str()) == Some("hy") {
            out.push(child);
        }
    }
    Ok(())
}

fn emit_parse_error(path: &Path, src: &str, message: &Message) {
    let config = ReportConfig::default();
    let mut sink = create_sink(&config, SourceMap::new(), Box::new(std::io::stderr()));
    let file_id = sink.register_source(path, src);
    sink.emit(reporting::Diagnostic::from_message(message, file_id));
    let _ = sink.finish();
}

fn format_one(path: &Path, check: bool) -> Result<bool, String> {
    let src = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let formatted = match format_source(&src) {
        Ok(s) => s,
        Err(msg) => {
            emit_parse_error(path, &src, &msg);
            return Err(format!("{}: parse error", path.display()));
        }
    };
    if formatted == src {
        return Ok(false);
    }
    if check {
        eprintln!("would reformat {}", path.display());
        return Ok(true);
    }
    // Atomic-ish write: temp beside target then rename.
    let tmp = path.with_extension("hy.fmt.tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.write_all(formatted.as_bytes())
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("rename {} -> {}: {e}", tmp.display(), path.display())
    })?;
    eprintln!("formatted {}", path.display());
    Ok(true)
}

fn main() {
    let raw: Vec<String> = std::env::args().collect();
    let args = match parse_args(&raw) {
        Ok(Some(args)) => args,
        Ok(None) => {
            let mut command = FmtArgs::command();
            command.set_bin_name("coil-fmt");
            let _ = command.print_long_help();
            println!();
            return;
        }
        Err(msg) => {
            if msg.ends_with('\n') {
                eprint!("{msg}");
            } else {
                eprintln!("{msg}");
            }
            exit(1);
        }
    };

    let mut files = Vec::new();
    for p in &args.paths {
        if let Err(e) = collect_hy_files(p, &mut files) {
            eprintln!("coil-fmt: {e}");
            exit(1);
        }
    }
    if files.is_empty() {
        eprintln!("coil-fmt: no `.hy` files found");
        exit(1);
    }
    files.sort();
    files.dedup();

    let mut changed = 0usize;
    let mut failed = 0usize;
    for path in &files {
        match format_one(path, args.check) {
            Ok(true) => changed += 1,
            Ok(false) => {}
            Err(e) => {
                eprintln!("coil-fmt: {e}");
                failed += 1;
            }
        }
    }

    if failed != 0 {
        exit(1);
    }
    if args.check && changed != 0 {
        eprintln!("coil-fmt: {changed} file(s) would be reformatted");
        exit(1);
    }
}
