//! DAP debug adapter server (stdio transport).

mod protocol;

use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use compiler::HostGrants;
use machine::StopReason;
use reporting::ReportConfig;
use serde_json::{Value, json};

use crate::session::DebugSession;

use protocol::{Message, read_message, write_message};

const THREAD_ID: i64 = 1;

struct DapServer {
    seq: i64,
    session: Option<DebugSession>,
    launched: bool,
    stop_on_entry: bool,
    pending_start: bool,
    exited: bool,
    /// Session started but paused before main (stopOnEntry).
    paused_at_entry: bool,
    /// Captures inferior stdout so it does not corrupt the DAP stdio stream.
    print_buf: Arc<Mutex<Vec<u8>>>,
}

impl DapServer {
    fn new() -> Self {
        let print_buf = Arc::new(Mutex::new(Vec::new()));
        machine::io::set_shared_print_redirect(Some(Arc::clone(&print_buf)));
        Self {
            seq: 1,
            session: None,
            launched: false,
            stop_on_entry: false,
            pending_start: false,
            exited: false,
            paused_at_entry: false,
            print_buf,
        }
    }

    fn next_seq(&mut self) -> i64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    fn send<W: Write>(&mut self, writer: &mut W, msg: Message) -> io::Result<()> {
        write_message(writer, &msg)
    }

    fn send_event<W: Write>(
        &mut self,
        writer: &mut W,
        event: &str,
        body: Option<Value>,
    ) -> io::Result<()> {
        let seq = self.next_seq();
        self.send(writer, Message::event(seq, event, body))
    }

    fn send_response<W: Write>(
        &mut self,
        writer: &mut W,
        request_seq: i64,
        command: &str,
        body: Value,
    ) -> io::Result<()> {
        let seq = self.next_seq();
        self.send(writer, Message::response(seq, request_seq, command, body))
    }

    fn send_error<W: Write>(
        &mut self,
        writer: &mut W,
        request_seq: i64,
        command: &str,
        message: &str,
    ) -> io::Result<()> {
        let seq = self.next_seq();
        self.send(
            writer,
            Message::error_response(seq, request_seq, command, message),
        )
    }

    fn handle_request<W: Write>(
        &mut self,
        writer: &mut W,
        msg: &Message,
    ) -> io::Result<bool> {
        let command = msg.command.as_deref().unwrap_or("");
        let request_seq = msg.seq;
        let args = msg.arguments.clone().unwrap_or(Value::Null);

        match command {
            "initialize" => {
                self.send_response(
                    writer,
                    request_seq,
                    command,
                    json!({
                        "supportsConfigurationDoneRequest": true,
                        "supportsFunctionBreakpoints": true,
                        "supportsStepBack": false,
                        "supportsRestartRequest": false,
                        "supportsSetVariable": false,
                        "supportsConditionalBreakpoints": false,
                        "supportsEvaluateForHovers": false,
                    }),
                )?;
                self.send_event(writer, "initialized", None)?;
            }
            "launch" => {
                let program = args
                    .get("program")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "launch requires program")
                    })?;
                let cwd = args
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from);
                let path = resolve_program_path(program, cwd.as_deref());
                let path_str = path.to_string_lossy().into_owned();
                let config = ReportConfig::from_cli_flags(false, false).map_err(|e| {
                    io::Error::new(io::ErrorKind::Other, e.to_string())
                })?;
                match DebugSession::compile(
                    config,
                    &path_str,
                    Box::new(io::stderr()),
                    HostGrants::deny_all(),
                    Vec::new(),
                ) {
                    Ok(mut session) => {
                        session.set_print_capture(Arc::clone(&self.print_buf));
                        self.session = Some(session);
                    }
                    Err(()) => {
                        self.send_error(writer, request_seq, command, "compile failed")?;
                        return Ok(true);
                    }
                }
                self.stop_on_entry = args
                    .get("stopOnEntry")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.launched = true;
                self.send_response(writer, request_seq, command, json!({}))?;
            }
            "setBreakpoints" => {
                let source = args
                    .get("source")
                    .and_then(|s| s.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let lines: Vec<u32> = args
                    .get("breakpoints")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|bp| bp.get("line").and_then(|l| l.as_u64()))
                            .map(|l| l as u32)
                            .collect()
                    })
                    .unwrap_or_default();
                let results = if let Some(session) = self.session.as_mut() {
                    session.replace_line_breakpoints(source, &lines)
                } else {
                    Vec::new()
                };
                let breakpoints: Vec<Value> = results
                    .into_iter()
                    .map(|r| {
                        json!({
                            "verified": r.verified,
                            "line": r.line,
                            "id": r.pc.unwrap_or(0),
                        })
                    })
                    .collect();
                self.send_response(
                    writer,
                    request_seq,
                    command,
                    json!({ "breakpoints": breakpoints }),
                )?;
            }
            "setFunctionBreakpoints" => {
                let names: Vec<String> = args
                    .get("breakpoints")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|bp| bp.get("name").and_then(|n| n.as_str()))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let result = self
                    .session
                    .as_mut()
                    .map(|s| s.replace_function_breakpoints(&refs))
                    .unwrap_or(Err("not launched".into()));
                let breakpoints = match result {
                    Ok(infos) => infos
                        .into_iter()
                        .map(|b| {
                            json!({
                                "verified": true,
                                "id": b.id,
                            })
                        })
                        .collect::<Vec<_>>(),
                    Err(e) => {
                        self.send_error(writer, request_seq, command, &e)?;
                        return Ok(true);
                    }
                };
                self.send_response(
                    writer,
                    request_seq,
                    command,
                    json!({ "breakpoints": breakpoints }),
                )?;
            }
            "configurationDone" => {
                self.pending_start = true;
                self.send_response(writer, request_seq, command, json!({}))?;
                if self.launched && self.pending_start {
                    self.pending_start = false;
                    self.start_or_stop_on_entry(writer)?;
                }
            }
            "threads" => {
                self.send_response(
                    writer,
                    request_seq,
                    command,
                    json!({
                        "threads": [{ "id": THREAD_ID, "name": "main" }]
                    }),
                )?;
            }
            "stackTrace" => {
                let frames = self
                    .session
                    .as_ref()
                    .map(DebugSession::stack_frames)
                    .unwrap_or_default();
                let stack: Vec<Value> = frames
                    .into_iter()
                    .map(|f| {
                        let source = f.path.as_ref().map(|p| json!({ "path": p }));
                        json!({
                            "id": f.index,
                            "name": f.name,
                            "line": f.line.unwrap_or(0),
                            "column": f.column.unwrap_or(0),
                            "source": source,
                        })
                    })
                    .collect();
                let total = stack.len();
                self.send_response(
                    writer,
                    request_seq,
                    command,
                    json!({ "stackFrames": stack, "totalFrames": total }),
                )?;
            }
            "scopes" => {
                let frame_id = args
                    .get("frameId")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as usize;
                let locals = self
                    .session
                    .as_ref()
                    .map(|s| s.locals_for_frame(frame_id))
                    .unwrap_or(Err("not launched".into()));
                let variables_ref = 1000 + frame_id as i64;
                match locals {
                    Ok(_) => {
                        self.send_response(
                            writer,
                            request_seq,
                            command,
                            json!({
                                "scopes": [{
                                    "name": "Locals",
                                    "variablesReference": variables_ref,
                                    "expensive": false,
                                }]
                            }),
                        )?;
                    }
                    Err(e) => self.send_error(writer, request_seq, command, &e)?,
                }
            }
            "variables" => {
                let variables_ref = args
                    .get("variablesReference")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let frame_id = variables_ref.saturating_sub(1000) as usize;
                let locals = self
                    .session
                    .as_ref()
                    .map(|s| s.locals_for_frame(frame_id))
                    .unwrap_or(Err("not launched".into()));
                match locals {
                    Ok(items) => {
                        let variables: Vec<Value> = items
                            .into_iter()
                            .enumerate()
                            .map(|(_i, loc)| {
                                json!({
                                    "name": loc.name,
                                    "value": loc.value,
                                    "variablesReference": 0,
                                })
                            })
                            .collect();
                        self.send_response(
                            writer,
                            request_seq,
                            command,
                            json!({ "variables": variables }),
                        )?;
                    }
                    Err(e) => self.send_error(writer, request_seq, command, &e)?,
                }
            }
            "continue" => {
                if self.paused_at_entry {
                    self.paused_at_entry = false;
                    let reason = self
                        .session
                        .as_mut()
                        .map(|s| s.start())
                        .unwrap_or(StopReason::Halt);
                    self.send_response(
                        writer,
                        request_seq,
                        command,
                        json!({ "allThreadsContinued": true }),
                    )?;
                    self.emit_stop_or_exit(writer, &reason)?;
                } else {
                    let reason = self
                        .session
                        .as_mut()
                        .map(|s| s.continue_exec())
                        .unwrap_or(Err("not launched".into()));
                    self.send_response(
                        writer,
                        request_seq,
                        command,
                        json!({ "allThreadsContinued": true }),
                    )?;
                    match reason {
                        Ok(r) => self.emit_stop_or_exit(writer, &r)?,
                        Err(e) => self.send_error(writer, request_seq, command, &e)?,
                    }
                }
            }
            "next" => {
                let reason = self
                    .session
                    .as_mut()
                    .map(|s| s.step_over())
                    .unwrap_or(Err("not launched".into()));
                self.send_response(writer, request_seq, command, json!({}))?;
                match reason {
                    Ok(r) => self.emit_stop_or_exit(writer, &r)?,
                    Err(e) => self.send_error(writer, request_seq, command, &e)?,
                }
            }
            "stepIn" => {
                let reason = self
                    .session
                    .as_mut()
                    .map(|s| s.step_in())
                    .unwrap_or(Err("not launched".into()));
                self.send_response(writer, request_seq, command, json!({}))?;
                match reason {
                    Ok(r) => self.emit_stop_or_exit(writer, &r)?,
                    Err(e) => self.send_error(writer, request_seq, command, &e)?,
                }
            }
            "stepOut" => {
                let reason = self
                    .session
                    .as_mut()
                    .map(|s| s.step_out())
                    .unwrap_or(Err("not launched".into()));
                self.send_response(writer, request_seq, command, json!({}))?;
                match reason {
                    Ok(r) => self.emit_stop_or_exit(writer, &r)?,
                    Err(e) => self.send_error(writer, request_seq, command, &e)?,
                }
            }
            "disconnect" | "terminate" => {
                self.send_response(writer, request_seq, command, json!({}))?;
                return Ok(false);
            }
            _ => {
                self.send_response(writer, request_seq, command, json!({}))?;
            }
        }
        Ok(true)
    }

    fn flush_captured_output<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        let bytes = {
            let mut guard = self
                .print_buf
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if guard.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut *guard)
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.send_event(
            writer,
            "output",
            Some(json!({
                "category": "stdout",
                "output": text,
            })),
        )
    }

    fn start_or_stop_on_entry<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.stop_on_entry {
            self.paused_at_entry = true;
            self.send_event(
                writer,
                "stopped",
                Some(json!({
                    "reason": "entry",
                    "threadId": THREAD_ID,
                    "allThreadsStopped": true,
                })),
            )?;
            return Ok(());
        }
        let reason = self
            .session
            .as_mut()
            .map(|s| s.start())
            .unwrap_or(StopReason::Halt);
        self.emit_stop_or_exit(writer, &reason)
    }

    fn emit_stop_or_exit<W: Write>(&mut self, writer: &mut W, reason: &StopReason) -> io::Result<()> {
        self.flush_captured_output(writer)?;
        match reason {
            StopReason::Halt => {
                if !self.exited {
                    self.exited = true;
                    self.send_event(writer, "exited", Some(json!({ "exitCode": 0 })))?;
                    self.send_event(writer, "terminated", None)?;
                }
            }
            StopReason::Panic => {
                if !self.exited {
                    self.exited = true;
                    self.send_event(writer, "output", Some(json!({
                        "category": "stderr",
                        "output": "Program panicked.\n",
                    })))?;
                    self.send_event(writer, "exited", Some(json!({ "exitCode": 1 })))?;
                    self.send_event(writer, "terminated", None)?;
                }
            }
            StopReason::Breakpoint { .. } => {
                self.send_event(
                    writer,
                    "stopped",
                    Some(json!({
                        "reason": "breakpoint",
                        "threadId": THREAD_ID,
                        "allThreadsStopped": true,
                    })),
                )?;
            }
            StopReason::Step | StopReason::Next | StopReason::Finish => {
                self.send_event(
                    writer,
                    "stopped",
                    Some(json!({
                        "reason": "step",
                        "threadId": THREAD_ID,
                        "allThreadsStopped": true,
                    })),
                )?;
            }
        }
        Ok(())
    }
}

fn resolve_program_path(program: &str, cwd: Option<&Path>) -> PathBuf {
    let p = PathBuf::from(program);
    if p.is_absolute() && p.exists() {
        return p;
    }
    if let Some(cwd) = cwd {
        let from_cwd = cwd.join(&p);
        if from_cwd.exists() {
            return from_cwd;
        }
    }
    if p.exists() {
        return p;
    }
    p
}

/// Run the DAP adapter on stdio until disconnect/terminate.
pub fn run_dap_server() -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut server = DapServer::new();

    loop {
        let Some(msg) = read_message(&mut reader)? else {
            break;
        };
        if msg.msg_type == "request" {
            let keep_going = server.handle_request(&mut stdout, &msg)?;
            if !keep_going {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::protocol::{Message, read_message, write_message};
    use super::resolve_program_path;
    use std::path::PathBuf;

    #[test]
    fn initialize_response_shape() {
        let resp = Message::response(
            2,
            1,
            "initialize",
            serde_json::json!({ "supportsConfigurationDoneRequest": true }),
        );
        assert_eq!(resp.success, Some(true));
        let mut buf = Vec::new();
        write_message(&mut buf, &resp).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let decoded = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(decoded.command.as_deref(), Some("initialize"));
    }

    #[test]
    fn resolve_program_path_prefers_cwd_relative() {
        let examples = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples");
        let resolved = resolve_program_path("fib.hy", Some(&examples));
        assert!(
            resolved.exists(),
            "cwd-relative fib.hy missing: {}",
            resolved.display()
        );
    }

    #[test]
    fn resolve_program_path_keeps_absolute_existing() {
        let abs = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../examples/fib.hy")
            .canonicalize()
            .unwrap();
        let resolved = resolve_program_path(abs.to_str().unwrap(), None);
        assert_eq!(resolved, abs);
    }
}
