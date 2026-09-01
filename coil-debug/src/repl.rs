//! GDB-style REPL front-end over [`DebugSession`].

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::process::exit;

use compiler::{format_bytecode_section, matches_fn_pat, HostGrants};
use machine::StopReason;
use reporting::{ReportConfig, ReportFormat};

use crate::session::{DebugSession, symbol_at_pc};

pub struct DebugArgs {
    pub filename: String,
    pub script: Option<String>,
    pub batch: bool,
    pub grants: HostGrants,
    pub extra_roots: Vec<std::path::PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
enum CmdResult {
    ContinuePrompt,
    Quit,
}

fn writer_for(format: ReportFormat) -> Box<dyn Write + Send> {
    match format {
        ReportFormat::Pretty => Box::new(std::io::stderr()),
        ReportFormat::Sarif | ReportFormat::Lsp => Box::new(std::io::stdout()),
    }
}

pub fn cmd_debug(config: ReportConfig, args: DebugArgs) {
    let format = config.format;
    let mut session = match DebugSession::compile(
        config,
        &args.filename,
        writer_for(format),
        args.grants,
        args.extra_roots,
    ) {
        Ok(s) => s,
        Err(()) => exit(1),
    };

    let batch = args.batch;
    let mut script_lines: Vec<String> = Vec::new();
    if let Some(ref path) = args.script {
        match fs::read_to_string(path) {
            Ok(s) => {
                for line in s.lines() {
                    script_lines.push(line.to_string());
                }
            }
            Err(e) => {
                eprintln!("debug: failed to read script `{path}`: {e}");
                exit(1);
            }
        }
    } else if batch {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => script_lines.push(l),
                Err(e) => {
                    eprintln!("debug: stdin read error: {e}");
                    exit(1);
                }
            }
        }
    }

    for line in &script_lines {
        match exec_line(&mut session, line, batch) {
            Ok(CmdResult::Quit) => {
                exit(if session.panicked() { 1 } else { 0 });
            }
            Ok(CmdResult::ContinuePrompt) => {}
            Err(e) => {
                eprintln!("debug: {e}");
                if batch {
                    exit(1);
                }
            }
        }
    }

    if batch {
        exit(if session.panicked() { 1 } else { 0 });
    }

    let stdin = io::stdin();
    loop {
        eprint!("(coil) ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("debug: read error: {e}");
                break;
            }
        }
        match exec_line(&mut session, &line, false) {
            Ok(CmdResult::Quit) => break,
            Ok(CmdResult::ContinuePrompt) => {}
            Err(e) => eprintln!("debug: {e}"),
        }
    }
    if session.panicked() {
        exit(1);
    }
}

fn exec_line(session: &mut DebugSession, line: &str, batch: bool) -> Result<CmdResult, String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(CmdResult::ContinuePrompt);
    }
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();

    match cmd {
        "help" | "h" => {
            print_help();
            Ok(CmdResult::ContinuePrompt)
        }
        "quit" | "q" => Ok(CmdResult::Quit),
        "break" | "b" => {
            let arg = rest
                .first()
                .copied()
                .ok_or("usage: break <fn|file:line|line>")?;
            let info = session.break_at(arg)?;
            println!("Breakpoint {} at {}", info.id, info.label);
            Ok(CmdResult::ContinuePrompt)
        }
        "delete" | "d" => {
            if rest.is_empty() {
                session.clear_breakpoints();
                println!("Deleted all breakpoints.");
            } else {
                let id: usize = rest[0]
                    .parse()
                    .map_err(|_| format!("invalid breakpoint id `{}`", rest[0]))?;
                session.delete_breakpoint(id)?;
                println!("Deleted breakpoint {id}.");
            }
            Ok(CmdResult::ContinuePrompt)
        }
        "info" => {
            let sub = rest.first().copied().unwrap_or("");
            match sub {
                "break" | "breakpoints" => {
                    let bps = session.breakpoints();
                    if bps.is_empty() {
                        println!("No breakpoints.");
                    } else {
                        println!("Num  Pc    What");
                        for b in bps {
                            println!("{:<4} {:<5} {}", b.id, b.pc, b.label);
                        }
                    }
                }
                "registers" | "reg" => {
                    let (ip, sp, depth) = session.registers();
                    println!("ip={ip}  sp={sp}  depth={depth}");
                }
                "locals" => cmd_info_locals(session)?,
                _ => return Err("usage: info break | info registers | info locals".into()),
            }
            Ok(CmdResult::ContinuePrompt)
        }
        "run" | "r" => {
            let reason = session.start();
            report_stop(session, &reason);
            if matches!(reason, StopReason::Panic) {
                return Err("program panicked".into());
            }
            Ok(CmdResult::ContinuePrompt)
        }
        "continue" | "c" => {
            let reason = session.continue_exec()?;
            report_stop(session, &reason);
            if batch && matches!(reason, StopReason::Panic) {
                return Err("program panicked".into());
            }
            Ok(CmdResult::ContinuePrompt)
        }
        "stepi" | "si" => {
            let reason = session.stepi()?;
            report_stop(session, &reason);
            if batch && matches!(reason, StopReason::Panic) {
                return Err("program panicked".into());
            }
            Ok(CmdResult::ContinuePrompt)
        }
        "step" | "s" => {
            let reason = session.step_in()?;
            report_stop(session, &reason);
            if batch && matches!(reason, StopReason::Panic) {
                return Err("program panicked".into());
            }
            Ok(CmdResult::ContinuePrompt)
        }
        "next" | "n" => {
            let reason = session.step_over()?;
            report_stop(session, &reason);
            if batch && matches!(reason, StopReason::Panic) {
                return Err("program panicked".into());
            }
            Ok(CmdResult::ContinuePrompt)
        }
        "finish" | "fin" => {
            let reason = session.step_out()?;
            report_stop(session, &reason);
            if batch && matches!(reason, StopReason::Panic) {
                return Err("program panicked".into());
            }
            Ok(CmdResult::ContinuePrompt)
        }
        "print" | "p" => {
            let arg = rest.first().copied().ok_or("usage: print <name|$N>")?;
            cmd_print(session, arg)?;
            Ok(CmdResult::ContinuePrompt)
        }
        "bt" | "backtrace" => {
            cmd_bt(session);
            Ok(CmdResult::ContinuePrompt)
        }
        "list" | "l" => {
            cmd_list(session)?;
            Ok(CmdResult::ContinuePrompt)
        }
        "disassemble" | "disas" | "dis" => {
            let pat = rest.first().copied();
            cmd_disas(session, pat)?;
            Ok(CmdResult::ContinuePrompt)
        }
        _ => Err(format!("unknown command `{cmd}` (try `help`)")),
    }
}

fn print_help() {
    println!(
        "Commands:\n\
         \x20 break / b <fn|file:line|line>  Set breakpoint\n\
         \x20 delete / d [n]                 Delete breakpoint(s)\n\
         \x20 info break | info registers    Status\n\
         \x20 run / r                        Start or restart\n\
         \x20 continue / c                   Resume\n\
         \x20 stepi / si                     Step one bytecode insn\n\
         \x20 step / s                       Step to next source line (into)\n\
         \x20 next / n                       Step over (same/outer depth)\n\
         \x20 finish / fin                   Run until frame returns\n\
         \x20 print / p <name|$N>           Print local by name or slot\n\
         \x20 info locals                   List named locals in current frame\n\
         \x20 bt / backtrace                 Call stack\n\
         \x20 list / l                       Source around stop\n\
         \x20 disassemble / disas [fn]       Bytecode dump\n\
         \x20 help / h                       This help\n\
         \x20 quit / q                       Exit"
    );
}

fn cmd_print(session: &DebugSession, arg: &str) -> Result<(), String> {
    let info = session.read_variable(arg)?;
    if info.name.starts_with('$') {
        println!("${} = {}", info.slot, info.value);
    } else {
        println!("{} (${}) = {}", info.name, info.slot, info.value);
    }
    Ok(())
}

fn cmd_info_locals(session: &DebugSession) -> Result<(), String> {
    if !session.started() {
        return Err("not started; use `run` first".into());
    }
    let depth = session.registers().2;
    if depth == 0 {
        return Err("no active frame".into());
    }
    let frame = depth - 1;
    let ip = session.current_ip();
    let fn_name = symbol_at_pc(&session.artifacts.functions, ip).unwrap_or("<unknown>");
    let locals = session.locals_for_frame(frame)?;
    if locals.is_empty() {
        println!("No named locals for {fn_name}.");
        return Ok(());
    }
    println!("Locals of {fn_name}:");
    for loc in locals {
        println!("  {} (${}) = {}", loc.name, loc.slot, loc.value);
    }
    Ok(())
}

fn cmd_bt(session: &DebugSession) {
    let frames = session.stack_frames();
    if frames.is_empty() {
        println!("No stack.");
        return;
    }
    let depth = frames.len();
    for (i, frame) in frames.iter().enumerate() {
        let loc = frame
            .path
            .as_ref()
            .zip(frame.line)
            .map(|(p, l)| format!(" at {p}:{l}"))
            .unwrap_or_default();
        println!(
            "#{:<2} {} pc={}{}",
            depth - 1 - i,
            frame.name,
            frame.pc,
            loc
        );
    }
}

fn cmd_list(session: &DebugSession) -> Result<(), String> {
    let (resolved, line, rows) = session.list_source()?;
    println!("{}:{}", resolved.display(), line);
    for (n, mark, src) in rows {
        let ch = if mark { '>' } else { ' ' };
        println!("{ch}{n:4}  {src}");
    }
    Ok(())
}

fn cmd_disas(session: &DebugSession, pat: Option<&str>) -> Result<(), String> {
    let arts = &session.artifacts;
    let pc_names: HashMap<usize, &str> = arts
        .functions
        .iter()
        .map(|s| (s.entry_pc as usize, s.name.as_str()))
        .collect();
    if let Some(p) = pat {
        let matched: Vec<_> = arts
            .functions
            .iter()
            .filter(|s| matches_fn_pat(&s.name, p))
            .collect();
        if matched.is_empty() {
            return Err(format!("no function matching `{p}`"));
        }
        let mut syms = arts.functions.clone();
        syms.sort_by_key(|s| s.entry_pc);
        let len = arts.bytecode.len();
        for (i, sym) in syms.iter().enumerate() {
            if !matches_fn_pat(&sym.name, p) {
                continue;
            }
            let start = sym.entry_pc as usize;
            let end = syms
                .get(i + 1)
                .map(|n| n.entry_pc as usize)
                .unwrap_or(len)
                .min(len);
            print!(
                "{}",
                format_bytecode_section(
                    &sym.name,
                    start,
                    end.max(start),
                    &arts.bytecode,
                    &arts.constants,
                    &pc_names,
                )
            );
        }
    } else {
        let ip = session.current_ip();
        let start = ip.saturating_sub(4);
        let end = (ip + 12).min(arts.bytecode.len());
        print!(
            "{}",
            format_bytecode_section(
                &format!("pc={ip}"),
                start,
                end,
                &arts.bytecode,
                &arts.constants,
                &pc_names,
            )
        );
    }
    Ok(())
}

fn report_stop(session: &DebugSession, reason: &StopReason) {
    let ip = session.current_ip();
    let sym = session.stop_symbol();
    let loc = session
        .stop_location()
        .map(|(p, l, _)| format!(" at {p}:{l}"))
        .unwrap_or_default();
    match reason {
        StopReason::Breakpoint { pc } => {
            let id = session.breakpoint_id_at_pc(*pc).unwrap_or(0);
            println!("Breakpoint {id}, {sym}{loc} (pc {pc})");
        }
        StopReason::Step => println!("Step, {sym}{loc} (pc {ip})"),
        StopReason::Next => println!("Next, {sym}{loc} (pc {ip})"),
        StopReason::Finish => println!("Finish, {sym}{loc} (pc {ip})"),
        StopReason::Halt => println!("Program exited normally."),
        StopReason::Panic => println!("Program panicked."),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_break_line_and_fn_shapes() {
        assert!("12".chars().all(|c| c.is_ascii_digit()));
        let (file, line) = "fib.hy:3".rsplit_once(':').unwrap();
        assert_eq!(file, "fib.hy");
        assert_eq!(line, "3");
    }
}
