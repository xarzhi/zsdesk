//! Shell icons and thumbnails.
//!
//! Tab, bookmark and address icons come from the system image list (same
//! icons Explorer shows). Desktop fences request icons/thumbnails from a
//! background STA thread through `IShellItemImageFactory`; completed bitmaps
//! remain in its queue and a generation-tagged message wakes the owner.

use std::{collections::{HashSet, VecDeque}, sync::{Arc, Condvar, Mutex, atomic::{AtomicUsize, Ordering}}, thread};

use windows::{
    core::{Interface, PCWSTR},
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, SIZE, WPARAM},
        Graphics::Gdi::{DeleteObject, GetObjectW, DIBSECTION, HBITMAP},
        Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
        System::{Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE}, Threading::GetCurrentThreadId},
        UI::{
            Controls::{IImageList, ILD_TRANSPARENT},
            Shell::{
                IShellItemImageFactory, SHGetFileInfoW, SHGetImageList, SHFILEINFOW, SHGFI_PIDL, SHGFI_SYSICONINDEX,
                SHIL_EXTRALARGE, SHIL_JUMBO, SHIL_LARGE, SHIL_SMALL, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT,
                SHGFI_USEFILEATTRIBUTES,
                DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass,
            },
            WindowsAndMessaging::{DestroyIcon, PostMessageW, HICON, WM_NCDESTROY},
        },
    },
};

use crate::gfx::BitmapAlpha;

/// System image list index for a PIDL.
pub fn icon_index(pidl: &[u8]) -> Option<i32> {
    let mut info = SHFILEINFOW::default();
    let ok = unsafe { SHGetFileInfoW(PCWSTR(pidl.as_ptr().cast()), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info),
        std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_PIDL | SHGFI_SYSICONINDEX) };
    (ok != 0).then_some(info.iIcon)
}

/// Icon index for a file name pattern without touching the disk.
///
/// With `SHGFI_USEFILEATTRIBUTES` the answer depends only on the extension and
/// on whether the item is a folder, and the call costs a few hundred
/// microseconds. A folder repeats a handful of extensions thousands of times,
/// so every answer is kept for the life of the process; listing a 17 000 entry
/// folder then costs the same as listing a small one.
pub fn icon_index_for_name(name: &str, directory: bool) -> Option<i32> {
    static CACHE: Mutex<Option<std::collections::HashMap<(String, bool), Option<i32>>>> = Mutex::new(None);
    let key = (name.rsplit_once('.').map(|(stem, extension)| if stem.is_empty() { "" } else { extension })
        .unwrap_or("").to_lowercase(), directory);
    if let Some(hit) = CACHE.lock().ok().and_then(|cache| cache.as_ref().and_then(|cache| cache.get(&key).copied())) {
        return hit;
    }
    let index = query_icon_index(name, directory);
    if let Ok(mut cache) = CACHE.lock() {
        cache.get_or_insert_with(std::collections::HashMap::new).insert(key, index);
    }
    index
}

fn query_icon_index(name: &str, directory: bool) -> Option<i32> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let attrs = if directory { windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY } else { windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL };
    let mut info = SHFILEINFOW::default();
    let ok = unsafe { SHGetFileInfoW(PCWSTR(wide.as_ptr()), attrs, Some(&mut info),
        std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES) };
    (ok != 0).then_some(info.iIcon)
}

/// Icon index of a real path, as the disk sees it.
///
/// A drive root has to be looked up on disk, not by name: `C:\` answers with
/// the hard-disk icon, a card reader with the removable-volume icon, and an
/// optical drive with a disc — three icons no file-name pattern can produce.
pub fn icon_index_for_path(path: &std::path::Path) -> Option<i32> {
    let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain(Some(0)).collect();
    let mut info = SHFILEINFOW::default();
    let ok = unsafe { SHGetFileInfoW(PCWSTR(wide.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info),
        std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX) };
    (ok != 0).then_some(info.iIcon)
}

/// Extracts an icon of at least `pixels` size from the system image list. Caller owns the HICON.
pub fn system_icon(index: i32, pixels: i32) -> Option<HICON> {
    let mut candidates = Vec::new();
    // SMALL/LARGE/EXTRALARGE already scale with DPI. Their nominal 16/32/48
    // names are not physical-pixel sizes on a high-DPI display.
    for list in [SHIL_SMALL, SHIL_LARGE, SHIL_EXTRALARGE, SHIL_JUMBO] {
        let Ok(images) = (unsafe { SHGetImageList::<IImageList>(list as i32) }) else { continue };
        let (mut width, mut height) = (0, 0);
        if unsafe { images.GetIconSize(&mut width, &mut height) }.is_err() { continue; }
        let Some(rank) = icon_size_rank(width, height, pixels) else { continue };
        if width == pixels && height == pixels {
            if let Ok(icon) = unsafe { images.GetIcon(index, ILD_TRANSPARENT.0) } {
                if !icon.is_invalid() { return Some(icon); }
            }
        } else { candidates.push((rank, images)); }
    }
    candidates.sort_by_key(|(rank, _)| *rank);
    candidates.into_iter().find_map(|(_, images)|
        unsafe { images.GetIcon(index, ILD_TRANSPARENT.0) }.ok().filter(|icon| !icon.is_invalid()))
}

fn icon_size_rank(width: i32, height: i32, requested: i32) -> Option<(bool, i32)> {
    let size = width.min(height);
    (size > 0).then(|| if size >= requested.max(1) { (false, size) } else { (true, -size) })
}

/// Extensions the list draws as a real picture instead of a generic icon.
///
/// Everything else — programs, documents, archives, shortcuts — keeps its own
/// Shell icon, so an `.exe` shows the icon it embeds rather than a page glyph.
const PICTURE: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "gif", "bmp", "dib", "webp", "tif", "tiff", "ico",
    "heic", "heif", "avif", "jxl", "raw", "cr2", "cr3", "nef", "arw", "dng", "orf", "rw2", "raf",
];
const VIDEO: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "avi", "wmv", "webm", "flv", "f4v", "mpg", "mpeg", "m2ts", "mts",
    "ts", "3gp", "3g2", "ogv", "rm", "rmvb", "vob", "asf", "divx",
];

/// Pictures and videos get a thumbnail; the Shell falls back to the file's icon
/// when no thumbnailer is installed or the file cannot be decoded.
pub fn wants_thumbnail(name: &str) -> bool {
    let Some((_, extension)) = name.rsplit_once('.') else { return false };
    let extension = extension.to_ascii_lowercase();
    PICTURE.contains(&extension.as_str()) || VIDEO.contains(&extension.as_str())
}

/// Owned icon handle.
pub struct OwnedIcon(pub HICON);impl Drop for OwnedIcon { fn drop(&mut self) { let _ = unsafe { DestroyIcon(self.0) }; } }

pub fn pidl_icon(pidl: &[u8], pixels: i32) -> Option<OwnedIcon> {
    system_icon(icon_index(pidl)?, pixels).map(OwnedIcon)
}

/// A finished icon or thumbnail.
pub struct IconResult {
    pub key: String,
    pub size: u32,
    pub token: u64,
    pub bitmap: HBITMAP,
    pub alpha: BitmapAlpha,
    pub pixel_bytes: u64,
}

impl IconResult {
    /// Releases the bitmap if the result is discarded.
    pub fn discard(self) {}
}
impl Drop for IconResult {
    fn drop(&mut self) { if !self.bitmap.is_invalid() { let _ = unsafe { DeleteObject(self.bitmap.into()) }; } }
}
// The bitmap has one owner and is never selected into a DC on the loader thread.
// GDI bitmap handles may be transferred within a process; the receiver either
// materializes its WIC copy or drops this result before anyone else uses it.
unsafe impl Send for IconResult {}

pub struct IconRequest { pub key: String, pub pidl: Vec<u8>, pub path: Option<std::path::PathBuf>, pub size: u32, pub thumbnail: bool, pub token: u64 }

impl IconRequest {
    /// Asks for a picture's thumbnail or a file's own icon, resolved from a
    /// filesystem path. `pidl` stays empty: the worker turns the path into a
    /// Shell item, so the UI thread never makes that cross-process call.
    pub fn for_path(key: String, path: std::path::PathBuf, size: u32, thumbnail: bool, token: u64) -> Self {
        Self { key, pidl: Vec::new(), path: Some(path), size, thumbnail, token }
    }
}

const MAX_PENDING: usize = 128;
const MAX_READY: usize = 16;
const MAX_READY_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Default)]
struct LoaderState {
    requests: VecDeque<IconRequest>,
    pending: HashSet<u64>,
    ready: VecDeque<IconResult>,
    ready_bytes: u64,
    stopping: bool,
}
impl LoaderState {
    fn enqueue(&mut self, request: IconRequest) -> bool {
        if self.stopping || self.pending.len() >= MAX_PENDING || !self.pending.insert(request.token) { return false; }
        self.requests.push_back(request);
        true
    }
    fn retain(&mut self, tokens: &HashSet<u64>) {
        self.pending.retain(|t| tokens.contains(t));
        self.requests.retain(|r| tokens.contains(&r.token));
        self.ready.retain(|r| tokens.contains(&r.token));
        self.ready_bytes = self.ready.iter().map(|r| r.pixel_bytes).sum();
    }
    fn stop(&mut self) {
        self.stopping = true;
        self.requests.clear(); self.pending.clear(); self.ready.clear(); self.ready_bytes = 0;
    }
    fn complete(&mut self, result: IconResult) -> bool {
        if self.stopping || !self.pending.contains(&result.token) { return false; }
        let post = self.ready.is_empty();
        self.ready_bytes = self.ready_bytes.saturating_add(result.pixel_bytes);
        self.ready.push_back(result);
        post
    }
}

#[derive(Clone, Copy)]
struct NotifyTarget { window: isize, message: u32, token: usize }
struct Notification {
    target: Mutex<Option<NotifyTarget>>,
    window: isize,
    owner_thread: u32,
    token: usize,
}
impl Notification {
    fn with_target<T>(&self, post: impl FnOnce(NotifyTarget) -> T) -> Option<T> {
        // WM_NCDESTROY and shutdown take the same short lock. No Shell/COM call
        // runs under it, and PostMessage does not wait for the target window.
        let target = self.target.lock().unwrap_or_else(|p| p.into_inner());
        target.map(post)
    }
    fn post(&self) -> bool {
        self.with_target(|target| unsafe { PostMessageW(Some(HWND(target.window as *mut _)), target.message, WPARAM(target.token), LPARAM(0)) }.is_ok()).unwrap_or(false)
    }
    fn revoke(&self) { *self.target.lock().unwrap_or_else(|p| p.into_inner()) = None; }
    fn detach(&self) {
        self.revoke();
        if self.owner_thread == unsafe { GetCurrentThreadId() }
            && unsafe { RemoveWindowSubclass(HWND(self.window as *mut _), Some(notification_proc), self.token) }.as_bool() {
            // The subclass owns exactly one Arc reference until removal/NCDESTROY.
            unsafe { drop(Arc::from_raw(self as *const Notification)); }
        }
    }
}

unsafe extern "system" fn notification_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM, id: usize, data: usize) -> LRESULT {
    if message == WM_NCDESTROY {
        let notification = unsafe { Arc::from_raw(data as *const Notification) };
        notification.revoke();
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(notification_proc), id) };
        return unsafe { DefSubclassProc(hwnd, message, wp, lp) };
    }
    unsafe { DefSubclassProc(hwnd, message, wp, lp) }
}

struct WorkerExit {
    shared: Arc<(Mutex<LoaderState>, Condvar)>, notification: Arc<Notification>, initialized: bool,
}
impl Drop for WorkerExit {
    fn drop(&mut self) {
        self.notification.revoke();
        self.shared.0.lock().unwrap_or_else(|p| p.into_inner()).stop();
        self.shared.1.notify_all();
        if self.initialized { unsafe { CoUninitialize() }; }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct IconLoaderStats { pub pending: usize, pub queued: usize, pub ready: usize, pub ready_pixel_bytes: u64 }

/// A bounded, cancellable queue. Window messages only wake the owner; this
/// object owns every completed bitmap, including results not consumed at exit.
pub struct IconLoader { shared: Arc<(Mutex<LoaderState>, Condvar)>, notification: Arc<Notification>, thread: Option<thread::JoinHandle<()>> }

impl IconLoader {
    /// WPARAM carries this loader's token; there is no bitmap/pointer payload.
    /// Validate it with `accepts_notification` before draining the result queue.
    pub fn spawn(hwnd: HWND, message: u32) -> Self {
        let shared = Arc::new((Mutex::new(LoaderState::default()), Condvar::new()));
        static NEXT_TOKEN: AtomicUsize = AtomicUsize::new(1);
        let token = loop { let value = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed); if value != 0 { break value; } };
        let notification = Arc::new(Notification { target: Mutex::new(Some(NotifyTarget { window: hwnd.0 as isize, message, token })),
            window: hwnd.0 as isize, owner_thread: unsafe { GetCurrentThreadId() }, token });
        let subclass = Arc::into_raw(notification.clone());
        if !unsafe { SetWindowSubclass(hwnd, Some(notification_proc), token, subclass as usize) }.as_bool() {
            unsafe { drop(Arc::from_raw(subclass)); }
            notification.revoke();
            shared.0.lock().unwrap_or_else(|p| p.into_inner()).stop();
            return Self { shared, notification, thread: None };
        }
        let worker = shared.clone();
        let notify = notification.clone();
        let thread = thread::Builder::new().name("zsdesk-icons".into()).spawn(move || {
            let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok();
            let _exit = WorkerExit { shared: worker.clone(), notification: notify.clone(), initialized };
            let (lock, wake) = &*worker;
            loop {
                let request = {
                    let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
                    while !state.stopping && state.requests.is_empty() { state = wake.wait(state).unwrap_or_else(|p| p.into_inner()); }
                    if state.stopping { break; }
                    state.requests.pop_front().unwrap()
                };
                let loaded = initialized.then(|| load(&request)).flatten();
                let (bitmap, alpha) = loaded.unwrap_or((HBITMAP::default(), BitmapAlpha::Ignore));
                let result = IconResult { key: request.key, size: request.size, token: request.token,
                    bitmap, alpha, pixel_bytes: bitmap_bytes(bitmap) };
                let should_post = {
                    let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
                    while !state.stopping && state.pending.contains(&result.token)
                        && (state.ready.len() >= MAX_READY || (!state.ready.is_empty() && state.ready_bytes.saturating_add(result.pixel_bytes) > MAX_READY_BYTES)) {
                        state = wake.wait(state).unwrap_or_else(|p| p.into_inner());
                    }
                    if state.stopping { break; }
                    if !state.pending.contains(&result.token) { continue; }
                    state.complete(result)
                };
                if should_post && !notify.post() {
                    lock.lock().unwrap_or_else(|p| p.into_inner()).stop();
                    wake.notify_all();
                    break;
                }
            }
        }).ok();
        if thread.is_none() { notification.detach();shared.0.lock().unwrap_or_else(|p| p.into_inner()).stop(); }
        Self { shared, notification, thread }
    }

    pub fn accepts_notification(&self, token: usize) -> bool {
        token == self.notification.token && !self.shared.0.lock().unwrap_or_else(|p| p.into_inner()).stopping
    }

    /// False means the bounded queue is full; retry on the next repaint.
    pub fn request(&self, request: IconRequest) -> bool {
        let accepted = self.shared.0.lock().unwrap_or_else(|p| p.into_inner()).enqueue(request);
        if accepted { self.shared.1.notify_one(); }
        accepted
    }

    pub fn take_ready(&self) -> Vec<IconResult> {
        let mut state = self.shared.0.lock().unwrap_or_else(|p| p.into_inner());
        let ready: Vec<_> = state.ready.drain(..).collect();
        for result in &ready { state.pending.remove(&result.token); }
        state.ready_bytes = 0;
        self.shared.1.notify_all();
        ready
    }

    /// Cancels queued/stale work without interrupting a Shell extension mid-call.
    pub fn retain_requests(&self, tokens: &HashSet<u64>) {
        self.shared.0.lock().unwrap_or_else(|p| p.into_inner()).retain(tokens);
        self.shared.1.notify_all();
    }

    pub fn stats(&self) -> IconLoaderStats {
        let state = self.shared.0.lock().unwrap_or_else(|p| p.into_inner());
        IconLoaderStats { pending: state.pending.len(), queued: state.requests.len(), ready: state.ready.len(), ready_pixel_bytes: state.ready_bytes }
    }

    pub fn shutdown(&mut self) {
        self.notification.detach();
        self.shared.0.lock().unwrap_or_else(|p| p.into_inner()).stop();
        self.shared.1.notify_all();
        // An in-flight third-party extractor may never return. Detach it after
        // revoking notifications; it owns only its stopped queue and COM scope.
        if let Some(thread) = self.thread.take() { if thread.is_finished() { let _ = thread.join(); } }
    }
}

impl Drop for IconLoader {
    fn drop(&mut self) { self.shutdown(); }
}

fn bitmap_bytes(bitmap: HBITMAP) -> u64 {
    if bitmap.is_invalid() { return 0; }
    let mut value = windows::Win32::Graphics::Gdi::BITMAP::default();
    let read = unsafe { GetObjectW(bitmap.into(), std::mem::size_of_val(&value) as i32, Some((&mut value as *mut windows::Win32::Graphics::Gdi::BITMAP).cast())) };
    if read <= 0 { 0 } else { u64::from(value.bmWidthBytes.unsigned_abs()) * u64::from(value.bmHeight.unsigned_abs()) }
}

fn load(request: &IconRequest) -> Option<(HBITMAP, BitmapAlpha)> {
    let item = match &request.path {
        Some(path) => super::pidl::item_from_parsing_name(&path.to_string_lossy()).ok()?,
        None => super::pidl::item_from_pidl(&request.pidl).ok()?,
    };
    let factory: IShellItemImageFactory = item.cast().ok()?;
    let size = SIZE { cx: request.size as i32, cy: request.size as i32 };
    let flags = if request.thumbnail { SIIGBF_RESIZETOFIT | SIIGBF_BIGGERSIZEOK } else { SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK };
    let bitmap = unsafe { factory.GetImage(size, flags).ok()? };
    Some((bitmap, classify_alpha(bitmap)))
}

/// Shell bitmaps may carry straight, premultiplied or no alpha; inspect the pixels.
fn classify_alpha(bitmap: HBITMAP) -> BitmapAlpha {
    let mut section = DIBSECTION::default();
    let read = unsafe { GetObjectW(bitmap.into(), std::mem::size_of::<DIBSECTION>() as i32, Some((&mut section as *mut DIBSECTION).cast())) };
    if read as usize != std::mem::size_of::<DIBSECTION>() || section.dsBm.bmBits.is_null() || section.dsBm.bmBitsPixel != 32 {
        return BitmapAlpha::Ignore;
    }
    let len = (section.dsBm.bmWidthBytes as usize) * (section.dsBm.bmHeight.unsigned_abs() as usize);
    let pixels = unsafe { std::slice::from_raw_parts(section.dsBm.bmBits.cast::<u8>(), len) };
    let mut any_alpha = false;
    for px in pixels.chunks_exact(4) {
        let a = px[3];
        if a != 0 { any_alpha = true; }
        if px[0] > a || px[1] > a || px[2] > a { return if any_alpha || a != 0 { BitmapAlpha::Straight } else { scan_rest(pixels) }; }
    }
    if any_alpha { BitmapAlpha::Premultiplied } else { BitmapAlpha::Ignore }
}

fn scan_rest(pixels: &[u8]) -> BitmapAlpha {
    if pixels.chunks_exact(4).any(|p| p[3] != 0) { BitmapAlpha::Straight } else { BitmapAlpha::Ignore }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_pictures_and_videos_ask_for_a_thumbnail() {
        assert!(wants_thumbnail("photo.PNG"));
        assert!(wants_thumbnail("clip.mp4"));
        assert!(wants_thumbnail("scan.tiff"));
        // Programs, documents and extension-less files keep their own icon.
        assert!(!wants_thumbnail("app.exe"));
        assert!(!wants_thumbnail("notes.txt"));
        assert!(!wants_thumbnail("LICENSE"));
        assert!(!wants_thumbnail("archive.tar.gz"));
        assert!(!wants_thumbnail(""));
    }
    #[test]
    fn icon_list_choice_uses_scaled_dimensions() {
        // At 150%, the small system list is already the exact 24px target.
        assert!(icon_size_rank(24,24,24) < icon_size_rank(48,48,24));
        assert!(icon_size_rank(32,32,20) < icon_size_rank(16,16,20));
        assert!(icon_size_rank(256,256,300) < icon_size_rank(48,48,300));
        assert_eq!(icon_size_rank(0,0,16), None);
    }
    fn request(token: u64) -> IconRequest { IconRequest { key: "same-file".into(), pidl: vec![0,0], path: None, size: 48, thumbnail: true, token } }
    #[test]
    fn pending_queue_is_bounded_and_cancellation_frees_capacity() {
        let mut state = LoaderState::default();
        for token in 1..=MAX_PENDING as u64 { assert!(state.enqueue(request(token))); }
        assert!(!state.enqueue(request(999)));
        state.retain(&HashSet::from([1, 2]));
        assert_eq!(state.requests.len(), 2);
        assert!(state.enqueue(request(999)));
    }
    #[test]
    fn cached_file_can_be_requested_again_after_completion_or_cancellation() {
        let mut state = LoaderState::default();
        assert!(state.enqueue(request(1)));
        state.retain(&HashSet::new());
        assert!(state.enqueue(request(2)));
        state.stop();
        assert!(state.pending.is_empty() && state.requests.is_empty());
        assert!(!state.enqueue(request(3)));
    }
    fn notification() -> Arc<Notification> {
        Arc::new(Notification { target: Mutex::new(Some(NotifyTarget { window: 7, message: 9, token: 11 })),
            window: 0, owner_thread: 0, token: 11 })
    }
    #[test]
    fn shutdown_revokes_late_wakes_and_discards_completions() {
        let notify = notification();
        let mut state = LoaderState::default();
        assert!(state.enqueue(request(1)));
        let in_flight = state.requests.pop_front().unwrap();
        notify.revoke();state.stop();
        assert!(notify.with_target(|_| panic!("late HWND notification")).is_none());
        assert!(!state.complete(IconResult { key: in_flight.key, size: 48, token: 1, bitmap: HBITMAP::default(),
            alpha: BitmapAlpha::Ignore, pixel_bytes: 0 }));
        assert!(state.ready.is_empty() && state.pending.is_empty());
    }
    #[test]
    fn worker_unwind_stops_its_queue_without_a_window_or_shell_call() {
        let shared = Arc::new((Mutex::new(LoaderState::default()), Condvar::new()));
        let notify = notification();
        shared.0.lock().unwrap().enqueue(request(1));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _exit = WorkerExit { shared: shared.clone(), notification: notify.clone(), initialized: false };
            panic!("simulated extractor failure");
        }));
        assert!(result.is_err());
        let state = shared.0.lock().unwrap();
        assert!(state.stopping && state.requests.is_empty() && state.pending.is_empty());
        assert!(notify.with_target(|_| ()).is_none());
    }
    #[test]
    fn shutdown_does_not_wait_for_an_in_flight_extractor() {
        use std::{sync::mpsc, time::Duration};
        let (release, waiting) = mpsc::channel();
        let (started, ready) = mpsc::channel();
        let (finished, exited) = mpsc::channel();
        let worker = thread::spawn(move || { started.send(()).unwrap(); let _ = waiting.recv(); let _ = finished.send(()); });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut loader = IconLoader { shared: Arc::new((Mutex::new(LoaderState::default()), Condvar::new())),
            notification: notification(), thread: Some(worker) };
        let (stopped, received) = mpsc::channel();
        let shutdown = thread::spawn(move || { loader.shutdown(); stopped.send(()).unwrap(); });
        let completed_without_release = received.recv_timeout(Duration::from_secs(2)).is_ok();
        let _ = release.send(());
        shutdown.join().unwrap();
        exited.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(completed_without_release);
    }
}
