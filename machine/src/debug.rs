//! GDB-style debug stop controller for the VM.
//!
//! Checks run only when a controller is attached (`unlikely`), so the normal
//! dispatch path stays cold when unused.

use std::collections::HashSet;

use crate::AddrHashBuilder;

/// Why the VM paused for the debugger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Breakpoint { pc: usize },
    Step,
    Next,
    Finish,
    Halt,
    Panic,
}

/// Single-step / finish bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepMode {
    None,
    /// Run one insn then stop before the next (`armed` flips after the first check).
    Stepi {
        armed: bool,
    },
    /// Stop when source line changes (step into).
    StepLine {
        file: u32,
        line: u32,
        start_depth: usize,
    },
    /// Stop when line changes at ≤ start depth (step over).
    Next {
        file: u32,
        line: u32,
        start_depth: usize,
    },
    /// Stop when frame depth drops to `target_depth` or below.
    Finish {
        target_depth: usize,
    },
}

/// Attached to a [`crate::Machine`] for interactive / scripted debugging.
#[derive(Debug, Clone, Default)]
pub struct DebugController {
    breakpoints: HashSet<usize, AddrHashBuilder>,
    step_mode: StepMode,
    /// Ignore a breakpoint at this PC once (continue / stepi from a hit).
    skip_bp_pc: Option<usize>,
}

impl Default for StepMode {
    fn default() -> Self {
        Self::None
    }
}

impl DebugController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn breakpoints(&self) -> &HashSet<usize, AddrHashBuilder> {
        &self.breakpoints
    }

    pub fn add_breakpoint(&mut self, pc: usize) -> bool {
        self.breakpoints.insert(pc)
    }

    pub fn remove_breakpoint(&mut self, pc: usize) -> bool {
        self.breakpoints.remove(&pc)
    }

    pub fn clear_breakpoints(&mut self) {
        self.breakpoints.clear();
    }

    pub fn set_stepi(&mut self) {
        self.step_mode = StepMode::Stepi { armed: false };
    }

    pub fn set_step_line(&mut self, file: u32, line: u32, start_depth: usize) {
        self.step_mode = StepMode::StepLine {
            file,
            line,
            start_depth,
        };
    }

    pub fn set_next(&mut self, file: u32, line: u32, start_depth: usize) {
        self.step_mode = StepMode::Next {
            file,
            line,
            start_depth,
        };
    }

    pub fn set_finish(&mut self, target_depth: usize) {
        self.step_mode = StepMode::Finish { target_depth };
    }

    pub fn clear_step(&mut self) {
        self.step_mode = StepMode::None;
    }

    pub fn clear_skip_bp(&mut self) {
        self.skip_bp_pc = None;
    }

    pub fn skip_breakpoint_once(&mut self, pc: usize) {
        self.skip_bp_pc = Some(pc);
    }

    /// Decide whether to stop **before** executing the insn at `ip`.
    ///
    /// `loc` is `(file_index, line)` when the PC has a known debug loc.
    pub fn check_stop(
        &mut self,
        ip: usize,
        depth: usize,
        loc: Option<(u32, u32)>,
    ) -> Option<StopReason> {
        // Stepi: first visit arms; second visit stops.
        if let StepMode::Stepi { armed } = self.step_mode {
            if armed {
                self.step_mode = StepMode::None;
                return Some(StopReason::Step);
            }
            self.step_mode = StepMode::Stepi { armed: true };
        }

        // Finish: stopped once we've returned to the caller.
        if let StepMode::Finish { target_depth } = self.step_mode
            && depth <= target_depth
        {
            self.step_mode = StepMode::None;
            return Some(StopReason::Finish);
        }

        // Breakpoints (after stepi arming so stepi from a BP still works).
        if self.breakpoints.contains(&ip) {
            if self.skip_bp_pc == Some(ip) {
                self.skip_bp_pc = None;
            } else {
                return Some(StopReason::Breakpoint { pc: ip });
            }
        }

        // Source line step / next.
        match self.step_mode {
            StepMode::StepLine {
                file,
                line,
                start_depth: _,
            } => {
                if let Some((f, l)) = loc
                    && (f != file || l != line)
                {
                    self.step_mode = StepMode::None;
                    return Some(StopReason::Step);
                }
            }
            StepMode::Next {
                file,
                line,
                start_depth,
            } => {
                if depth < start_depth {
                    self.step_mode = StepMode::None;
                    return Some(StopReason::Next);
                }
                if depth == start_depth
                    && let Some((f, l)) = loc
                    && (f != file || l != line)
                {
                    self.step_mode = StepMode::None;
                    return Some(StopReason::Next);
                }
            }
            _ => {}
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepi_stops_on_second_check() {
        let mut c = DebugController::new();
        c.set_stepi();
        assert_eq!(c.check_stop(10, 1, None), None);
        assert_eq!(c.check_stop(11, 1, None), Some(StopReason::Step));
    }

    #[test]
    fn breakpoint_hits_and_skip_once() {
        let mut c = DebugController::new();
        c.add_breakpoint(5);
        assert_eq!(
            c.check_stop(5, 1, None),
            Some(StopReason::Breakpoint { pc: 5 })
        );
        c.skip_breakpoint_once(5);
        assert_eq!(c.check_stop(5, 1, None), None);
        assert_eq!(
            c.check_stop(5, 1, None),
            Some(StopReason::Breakpoint { pc: 5 })
        );
    }

    #[test]
    fn next_stops_on_line_change_at_same_depth() {
        let mut c = DebugController::new();
        c.set_next(0, 3, 2);
        assert_eq!(c.check_stop(1, 2, Some((0, 3))), None);
        assert_eq!(c.check_stop(2, 3, Some((0, 99))), None); // inside callee
        assert_eq!(c.check_stop(3, 2, Some((0, 4))), Some(StopReason::Next));
    }

    #[test]
    fn next_stops_when_returning_below_start_depth() {
        let mut c = DebugController::new();
        c.set_next(0, 3, 2);
        assert_eq!(c.check_stop(1, 1, Some((0, 3))), Some(StopReason::Next));
    }

    #[test]
    fn step_line_stops_on_line_or_file_change() {
        let mut c = DebugController::new();
        c.set_step_line(0, 3, 1);
        assert_eq!(c.check_stop(1, 1, Some((0, 3))), None);
        assert_eq!(c.check_stop(2, 2, Some((0, 3))), None); // into callee, same line
        assert_eq!(c.check_stop(3, 2, Some((0, 4))), Some(StopReason::Step));

        c.set_step_line(0, 3, 1);
        assert_eq!(c.check_stop(4, 1, Some((1, 3))), Some(StopReason::Step));
    }

    #[test]
    fn finish_stops_at_or_below_target_depth() {
        let mut c = DebugController::new();
        c.set_finish(1);
        assert_eq!(c.check_stop(10, 2, None), None);
        assert_eq!(c.check_stop(11, 1, None), Some(StopReason::Finish));

        c.set_finish(1);
        assert_eq!(c.check_stop(12, 0, None), Some(StopReason::Finish));
    }

    #[test]
    fn stepi_from_breakpoint_skip_then_step() {
        let mut c = DebugController::new();
        c.add_breakpoint(5);
        c.set_stepi();
        c.skip_breakpoint_once(5);
        // First visit: arm stepi and consume skip; do not stop.
        assert_eq!(c.check_stop(5, 1, None), None);
        assert_eq!(c.check_stop(6, 1, None), Some(StopReason::Step));
    }
}
