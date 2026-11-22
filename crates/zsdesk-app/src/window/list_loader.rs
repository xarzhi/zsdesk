//! Background folder listing.
//!
//! Reading a folder is the only part of navigation that touches the disk, and a
//! big folder or a slow share would otherwise leave the window frozen with an
//! empty pane until it finished. The listing therefore runs on a worker thread
//! and the window picks the result up from its own message queue: the pane
//! shows "正在读取…", the chrome stays responsive, and a second navigation
//! simply supersedes the first.

use std::sync::{Arc, Mutex};

use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};
use zsdesk_core::model::explorer::ShellLocation;
use zsdesk_windows::shell::folder_items::{self, FolderEntry};

/// A worker finished a listing. Carries no payload, so a message that outlives
/// its window owns nothing.
pub(super) const WM_VIEW_LISTED: u32 = WM_APP + 48;

/// One finished listing. `token` is the request the view is waiting for.
pub(super) struct Listing {
    pub token: u64,
    pub items: Result<Vec<FolderEntry>, String>,
}

/// Hands folder listings to worker threads and collects their results.
#[derive(Default)]
pub(super) struct ListLoader {
    hwnd: isize,
    finished: Arc<Mutex<Vec<Listing>>>,
}

impl ListLoader {
    pub(super) fn attach(&mut self, hwnd: HWND) { self.hwnd = hwnd.0 as isize; }

    /// Enumerates `location` on a worker thread. The result is collected by
    /// `drain` after `WM_VIEW_LISTED` arrives. `include_hidden` mirrors the
    /// 查看 → 隐藏的项目 switch, which the Shell applies to its own view.
    pub(super) fn start(&self, token: u64, location: ShellLocation, include_hidden: bool) {
        let finished = self.finished.clone();
        let hwnd = self.hwnd;
        // Only the fallback below needs the location after the worker took it.
        let retry = location.clone();
        let worker = std::thread::Builder::new().name("zsdesk-list".into()).spawn(move || {
            // The Shell side of a namespace folder needs an apartment of its
            // own and must never touch the window's one.
            let items = unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let listed = folder_items::list(&location, include_hidden).map_err(|error| error.to_string());
                CoUninitialize();
                listed
            };
            if let Ok(mut slot) = finished.lock() { slot.push(Listing { token, items }); }
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_VIEW_LISTED, WPARAM(0), LPARAM(0)) };
        });
        if worker.is_err() {
            // Out of threads: the caller's request still has to be answered, so
            // list on this thread rather than leave the pane waiting forever.
            let items = unsafe { folder_items::list(&retry, include_hidden).map_err(|error| error.to_string()) };
            if let Ok(mut slot) = self.finished.lock() { slot.push(Listing { token, items }); }
            let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_VIEW_LISTED, WPARAM(0), LPARAM(0)) };
        }
    }

    /// Describes a batch of paths on a worker thread, for search results: the
    /// engine already produced them, so there is no folder to enumerate — only
    /// metadata to read before the rows can be drawn.
    pub(super) fn start_paths(&self, token: u64, paths: Vec<std::path::PathBuf>, include_hidden: bool) {
        let finished = self.finished.clone();
        let hwnd = self.hwnd;
        let retry = paths.clone();
        let worker = std::thread::Builder::new().name("zsdesk-search-rows".into()).spawn(move || {
            let items = unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let rows = folder_items::entries_for_paths(&paths, include_hidden);
                CoUninitialize();
                Ok(rows)
            };
            if let Ok(mut slot) = finished.lock() { slot.push(Listing { token, items }); }
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_VIEW_LISTED, WPARAM(0), LPARAM(0)) };
        });
        if worker.is_err() {
            let items = unsafe { Ok(folder_items::entries_for_paths(&retry, include_hidden)) };
            if let Ok(mut slot) = self.finished.lock() { slot.push(Listing { token, items }); }
            let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_VIEW_LISTED, WPARAM(0), LPARAM(0)) };
        }
    }

    /// Takes every listing that finished since the last call.
    pub(super) fn drain(&self) -> Vec<Listing> {
        self.finished.lock().map(|mut slot| std::mem::take(&mut *slot)).unwrap_or_default()
    }
}
