//! Drag source for the items ZSDesk paints itself.
//!
//! The rows are drawn by hand, so the Shell view never gets a chance to start a
//! drag of its own. This module hands a Shell data object to `DoDragDrop` with
//! a minimal `IDropSource`; Explorer, the desktop and every other drop target
//! then treat the items exactly as they treat a drag from File Explorer.

use windows::{
    core::{implement, BOOL, HRESULT, Result},
    Win32::{
        Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND},
        System::{
            Com::IDataObject,
            Ole::{DoDragDrop, IDropSource, IDropSource_Impl, DROPEFFECT, DROPEFFECT_NONE},
            SystemServices::{MK_LBUTTON, MK_MBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS},
        },
    },
};

/// Tells `DoDragDrop` when the drag ends: Escape cancels, releasing every mouse
/// button drops. The default cursors are what every drag source uses.
#[implement(IDropSource)]
struct Source;

#[allow(non_snake_case)]
impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() { return DRAGDROP_S_CANCEL; }
        let buttons = MK_LBUTTON.0 | MK_RBUTTON.0 | MK_MBUTTON.0;
        if keys.0 & buttons == 0 { return DRAGDROP_S_DROP; }
        HRESULT(0)
    }
    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT { DRAGDROP_S_USEDEFAULTCURSORS }
}

/// Runs a drag of `pidls` and returns the effect the target applied.
///
/// # Safety
///
/// OLE must be initialized on this thread and `pidls` must be absolute shell
/// item id lists.
pub unsafe fn begin(owner: HWND, pidls: &[Vec<u8>], allowed: DROPEFFECT) -> Result<DROPEFFECT> {
    let data: IDataObject = crate::shell::menu::items_ui_object(owner, pidls)?;
    let source: IDropSource = Source.into();
    let mut effect = DROPEFFECT_NONE;
    let _ = unsafe { DoDragDrop(&data, &source, allowed, &mut effect) };
    Ok(effect)
}
