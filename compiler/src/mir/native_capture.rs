//! Hand each body's MIR to a native backend (`coil-native`).
//!
//! While a capture is open (`coil --jit`), `IlModule::optimize_and_flatten`
//! copies every body's MIR, as lowered from HIR, before the VM tiers take
//! it. The backend reads the bodies it can compile and maps them to the
//! bytecode entry PCs by name.

use std::cell::RefCell;

use super::MirFunc;
use crate::il::Label;

/// One function body's MIR.
#[derive(Clone, Debug)]
pub struct NativeBody {
    /// Function name, as in the compiler's entry table.
    pub name: String,
    /// Label `CALL`s of this body target (`MirInst::Call::target`).
    pub entry: Option<Label>,
    pub mir: MirFunc,
}

thread_local! {
    static NATIVE_CAPTURE: RefCell<Option<Vec<NativeBody>>> = const { RefCell::new(None) };
}

/// Start capturing (drops an earlier capture).
pub fn start_native_capture() {
    NATIVE_CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

pub fn native_capture_active() -> bool {
    NATIVE_CAPTURE.with(|c| c.borrow().is_some())
}

/// End the capture and return its bodies.
pub fn take_native_capture() -> Vec<NativeBody> {
    NATIVE_CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default()
}

/// Record `bodies` when a capture is open.
pub fn capture_native_bodies<'a>(bodies: impl Iterator<Item = (&'a str, Option<Label>, &'a MirFunc)>) {
    NATIVE_CAPTURE.with(|c| {
        if let Some(list) = c.borrow_mut().as_mut() {
            list.extend(bodies.map(|(name, entry, mir)| NativeBody {
                name: name.to_string(),
                entry,
                mir: mir.clone(),
            }));
        }
    });
}
