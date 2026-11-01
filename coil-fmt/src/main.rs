//! `coil-fmt` — format `.coil` sources (single file or directory).
//!
//! Preserves `//` comments and `///` docs attached to declarations. Default
//! rewrites in place; `--check` exits non-zero when reformatting would change a file.
//! A legacy `.hy` source is written to its `.coil` name and the `.hy` removed.

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
    after_help = "Directories are walked recursively for `*.coil` (and legacy `*.hy`).\n\
A legacy `.hy` file is written to the `.coil` name beside it and the old file removed;\n\
`--check` reports it as a change.\n\
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

fn collect_sources(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if root.is_file() {
        if common::ext::is_source_path(root) {
            out.push(root.to_path_buf());
        } else {
            return Err(format!("`{}` is not a `.coil` file", root.display()));
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
            // Deliberately broken programs (`coil test` expects them to be
            // rejected); a path naming one directly is still formatted.
            if child.file_name().is_some_and(|n| n == "compile_fail") {
                continue;
            }
            collect_sources(&child, out)?;
        } else if common::ext::is_source_path(&child) {
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

/// Where `path` is written: a legacy `.hy` source moves to the `.coil` name
/// beside it, anything else stays put.
fn target_path(path: &Path) -> PathBuf {
    if path
        .extension()
        .is_some_and(|e| e == common::ext::LEGACY_SOURCE_EXT)
    {
        path.with_extension(common::ext::SOURCE_EXT)
    } else {
        path.to_path_buf()
    }
}

/// Format `path`; a legacy `.hy` file is written to its `.coil` name and the
/// old file removed. Returns whether anything changed (or would, with `check`).
fn format_one(path: &Path, check: bool) -> Result<bool, String> {
    let target = target_path(path);
    let moving = target != path;
    if moving && target.exists() {
        return Err(format!(
            "{} already exists; not moving {} onto it",
            target.display(),
            path.display()
        ));
    }
    let src = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let formatted = match format_source(&src) {
        Ok(s) => s,
        Err(msg) => {
            emit_parse_error(path, &src, &msg);
            return Err(format!("{}: parse error", path.display()));
        }
    };
    let reformat = formatted != src;
    if !reformat && !moving {
        return Ok(false);
    }
    if check {
        if moving {
            eprintln!("would rename {} -> {}", path.display(), target.display());
        }
        if reformat {
            eprintln!("would reformat {}", path.display());
        }
        return Ok(true);
    }
    // Atomic-ish write: temp beside target then rename.
    let tmp = target.with_extension("fmt.tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.write_all(formatted.as_bytes())
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, &target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("rename {} -> {}: {e}", tmp.display(), target.display())
    })?;
    if moving {
        fs::remove_file(path).map_err(|e| format!("remove {}: {e}", path.display()))?;
        eprintln!("formatted {} -> {}", path.display(), target.display());
    } else {
        eprintln!("formatted {}", path.display());
    }
    Ok(true)
}

fn main() {
    coil_build_id::check_parent("coil-fmt");
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
        if let Err(e) = collect_sources(p, &mut files) {
            eprintln!("coil-fmt: {e}");
            exit(1);
        }
    }
    if files.is_empty() {
        eprintln!("coil-fmt: no `.coil` files found");
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

#[cfg(test)]
mod tests {
    use super::*;
    use common::ext::{LEGACY_SOURCE_EXT, SOURCE_EXT};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coil-fmt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const MESSY: &str = "fn main(){let x=1;}\n";

    #[test]
    fn legacy_file_is_formatted_into_coil_file() {
        let dir = scratch("move");
        let old = dir.join("a").with_extension(LEGACY_SOURCE_EXT);
        fs::write(&old, MESSY).unwrap();
        assert!(format_one(&old, false).unwrap());
        let new = dir.join("a").with_extension(SOURCE_EXT);
        assert!(!old.exists(), "legacy file should be gone");
        let out = fs::read_to_string(&new).unwrap();
        assert_eq!(out, format_source(MESSY).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn already_formatted_legacy_file_is_still_renamed() {
        let dir = scratch("clean");
        let old = dir.join("b").with_extension(LEGACY_SOURCE_EXT);
        let clean = format_source(MESSY).unwrap();
        fs::write(&old, &clean).unwrap();
        assert!(format_one(&old, false).unwrap());
        assert!(!old.exists());
        assert_eq!(fs::read_to_string(dir.join("b.coil")).unwrap(), clean);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_reports_legacy_file_without_touching_it() {
        let dir = scratch("check");
        let old = dir.join("c").with_extension(LEGACY_SOURCE_EXT);
        fs::write(&old, format_source(MESSY).unwrap()).unwrap();
        assert!(
            format_one(&old, true).unwrap(),
            "a legacy name needs a change"
        );
        assert!(old.exists());
        assert!(!dir.join("c.coil").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_file_is_not_moved_over_an_existing_coil_file() {
        let dir = scratch("clash");
        let old = dir.join("d").with_extension(LEGACY_SOURCE_EXT);
        let new = dir.join("d.coil");
        fs::write(&old, MESSY).unwrap();
        fs::write(&new, "fn kept() {}\n").unwrap();
        assert!(format_one(&old, false).is_err());
        assert_eq!(fs::read_to_string(&old).unwrap(), MESSY);
        assert_eq!(fs::read_to_string(&new).unwrap(), "fn kept() {}\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn coil_file_is_formatted_in_place() {
        let dir = scratch("inplace");
        let path = dir.join("e.coil");
        fs::write(&path, MESSY).unwrap();
        assert!(format_one(&path, false).unwrap());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format_source(MESSY).unwrap()
        );
        assert!(!format_one(&path, false).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }
}
