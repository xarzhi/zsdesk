//! Handing a dragged tab to another file window.
//!
//! A tab strip drag that ends over another window merges the two: the source
//! window sends the whole tab as JSON through `WM_COPYDATA`, and the receiving
//! window answers `1` once it queued the tab, so the source only drops its copy
//! after the tab has a new home.

use std::path::Path;

use windows::{
    core::{BOOL, PCWSTR},
    Win32::{
        Foundation::{HWND, LPARAM, POINT, RECT, WPARAM},
        System::DataExchange::COPYDATASTRUCT,
        UI::WindowsAndMessaging::*,
    },
};
use zsdesk_core::model::explorer::ExplorerTab;

/// Tags a `WM_COPYDATA` packet that carries a tab.
pub(super) const MAGIC: usize = 0x5a53_5454;
/// Posted to the receiving window once its `WM_COPYDATA` accepted the tab.
/// The id must stay clear of every other private message: the run loop filters
/// some of them by value before the window procedure sees them.
pub(super) const WM_TAB_TRANSFER: u32 = WM_APP + 47;

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Transfer {
    pub tab: ExplorerTab,
    /// Screen point where the tab was released, so the receiving strip can
    /// insert it where the user aimed instead of always at the end.
    pub at: Option<(i32, i32)>,
}

/// The file window of this profile under `screen`, excluding `own`. Windows of
/// another profile are other users' data and are left alone.
///
/// The topmost window at the point decides, so a file window hidden behind
/// another application is not a drop target. Enumeration is used instead of
/// `WindowFromPoint` because the drag card itself follows the pointer.
pub(super) fn target(profile: &Path, screen: POINT, own: HWND) -> Option<HWND> {
    let mut find = Find { point: screen, own, topmost: HWND::default() };
    // The callback stops at the first window whose rectangle contains the point;
    // `EnumWindows` walks the Z-order from top to bottom.
    let _ = unsafe { EnumWindows(Some(topmost_at), LPARAM(&mut find as *mut Find as isize)) };
    if find.topmost.0.is_null() || find.topmost == own { return None; }
    let key = crate::instance::property(profile);
    (!unsafe { GetPropW(find.topmost, PCWSTR(key.as_ptr())) }.0.is_null()).then_some(find.topmost)
}

struct Find { point: POINT, own: HWND, topmost: HWND }

unsafe extern "system" fn topmost_at(hwnd: HWND, data: LPARAM) -> BOOL {
    let find = unsafe { &mut *(data.0 as *mut Find) };
    if hwnd == find.own { return BOOL(1); }
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() { return BOOL(1); }
    // Tool and transparent windows (the drag card, tooltips, palettes) never
    // receive a drop and must not hide the window underneath.
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_TRANSPARENT.0) != 0 { return BOOL(1); }
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() { return BOOL(1); }
    if find.point.x < rect.left || find.point.y < rect.top
        || find.point.x >= rect.right || find.point.y >= rect.bottom { return BOOL(1); }
    find.topmost = hwnd;
    BOOL(0)
}

/// Sends `transfer` to `target`; true when that window took the tab.
pub(super) fn send(target: HWND, transfer: &Transfer) -> bool {
    let Ok(bytes) = serde_json::to_vec(transfer) else { return false };
    let data = COPYDATASTRUCT { dwData: MAGIC, cbData: bytes.len() as u32, lpData: bytes.as_ptr() as *mut _ };
    let mut accepted = 0usize;
    // The receiver only queues the tab, so a short timeout is enough; a hung
    // window must not freeze the drag.
    let sent = unsafe {
        SendMessageTimeoutW(target, WM_COPYDATA, WPARAM(0), LPARAM(&data as *const _ as isize),
            SMTO_ABORTIFHUNG, 2000, Some(&mut accepted))
    };
    sent.0 != 0 && accepted == 1
}
