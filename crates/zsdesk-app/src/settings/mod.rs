//! Fluent settings dialog.
//!
//! The title-strip gear opens one owned popup window that hosts every
//! preference. The dialog edits the user interface block of the saved session
//! on its own (`save_window(.., secondary = true)`) and asks the file-manager
//! window to pick the change up, so both windows keep working from one copy of
//! the preferences. Actions that need the shell window (index rebuild, routing
//! setup, system integration dialogs) are sent to it as a one-line command.

mod model;
mod paint;

use std::{cell::RefCell, path::{Path, PathBuf}};
use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{
        BeginPaint, BitBlt, ClientToScreen, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST,
        MONITORINFO, MonitorFromRect, PAINTSTRUCT, SRCCOPY,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::WM_MOUSELEAVE,
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
            VK_DOWN, VK_ESCAPE, VK_NEXT, VK_PRIOR, VK_UP},
        WindowsAndMessaging::*,
    },
};
use zsdesk_core::model::explorer::{BackdropStyle, ContextMenuPreferences, ExplorerUi};
use zsdesk_ui::{menus, theme::{Mode, Palette}};
use zsdesk_windows::gfx::{Gfx, Surface, TextStyle};

use model::{Layout, Section, Target};

pub const CLASS: PCWSTR = windows::core::w!("ZSDesk.Native.Settings");
/// Posted to the file-manager window after the dialog saved new preferences.
pub const WM_SETTINGS_CHANGED: u32 = WM_APP + 61;
/// Sent to the file-manager window with a `Box<String>` action id.
pub const WM_SETTINGS_ACTION: u32 = WM_APP + 62;

const TITLE: &str = "设置";
// One dialog per process; the gear focuses it instead of stacking copies.
thread_local! {
    static DIALOG: RefCell<Option<Dialog>> = const { RefCell::new(None) };
}

fn wide(text: &str) -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() }

/// Opens the dialog, or brings the existing one to the front.
pub fn open(parent: HWND, config_path: &Path) -> anyhow::Result<()> {
    if let Ok(existing) = unsafe { FindWindowW(CLASS, PCWSTR::null()) } {
        if !existing.0.is_null() {
            unsafe { let _ = ShowWindow(existing, SW_SHOWNORMAL); let _ = SetForegroundWindow(existing); let _ = SetFocus(Some(existing)); }
            return Ok(());
        }
    }
    let instance = HINSTANCE(unsafe { GetModuleHandleW(None)? }.0);
    unsafe {
        RegisterClassW(&WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS,
            ..Default::default()
        });
    }
    let mut dialog = Dialog::new(parent, config_path)?;
    let dpi = unsafe { GetDpiForWindow(parent) }.max(96);
    let width = model::scale(model::DIALOG_W, dpi) as i32;
    let height = model::scale(model::DIALOG_H, dpi) as i32;
    let (x, y) = center_on(parent, width, height);
    let hwnd = unsafe {
        CreateWindowExW(WINDOW_EX_STYLE::default(), CLASS, PCWSTR(wide(TITLE).as_ptr()),
            WS_POPUP | WS_CLIPCHILDREN | WS_SYSMENU, x, y, width, height, Some(parent), None, Some(instance), None)?
    };
    dialog.hwnd = hwnd;
    dialog.dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    dialog.apply_frame();
    dialog.layout();
    DIALOG.with(|slot| *slot.borrow_mut() = Some(dialog));
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    Ok(())
}

fn center_on(parent: HWND, width: i32, height: i32) -> (i32, i32) {
    let mut bounds = RECT::default();
    if unsafe { GetWindowRect(parent, &mut bounds) }.is_err() {
        bounds = RECT { left: 0, top: 0, right: 1280, bottom: 800 };
    }
    let mut work = bounds;
    let monitor = unsafe { MonitorFromRect(&bounds, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() { work = info.rcWork; }
    let x = bounds.left + (bounds.right - bounds.left - width) / 2;
    let y = bounds.top + (bounds.bottom - bounds.top - height) / 2;
    (x.clamp(work.left, (work.right - width).max(work.left)), y.clamp(work.top, (work.bottom - height).max(work.top)))
}

struct Dialog {
    hwnd: HWND,
    parent: HWND,
    config_path: PathBuf,
    ui: ExplorerUi,
    sections: Vec<Section>,
    section: usize,
    scroll: f32,
    layout: Layout,
    hover: Target,
    pressed: Target,
    /// Cursor position in screen pixels and the window origin when the header is
    /// being dragged. The dialog owns the drag so it works without a caption or
    /// a thick frame.
    drag: Option<(POINT, i32, i32)>,
    tracking: bool,
    dark: bool,
    active: bool,
    dpi: u32,
    gfx: Option<Gfx>,
    surface: Option<Surface>,
    message: Option<String>,
}

impl Dialog {
    fn new(parent: HWND, config_path: &Path) -> anyhow::Result<Self> {
        let mut ui = crate::storage::load(config_path).map(|session| session.ui).unwrap_or_default();
        if !crate::edition::THEMED {
            // Only the themed edition honours the material preferences.
            ui.appearance = Default::default();
        }
        let mut dialog = Self {
            hwnd: HWND::default(), parent, config_path: config_path.to_path_buf(), ui,
            sections: Vec::new(), section: 0, scroll: 0.0,
            layout: model::layout(1, 1, 96, &[], 0, 0.0, &|_, _, _| 0.0),
            hover: Target::None, pressed: Target::None, drag: None, tracking: false,
            dark: false, active: true, dpi: 96, gfx: None, surface: None, message: None,
        };
        dialog.rebuild();
        Ok(dialog)
    }

    /// Reloads the preferences, then rebuilds the section list.
    fn reload(&mut self) {
        if let Ok(session) = crate::storage::load(&self.config_path) { self.ui = session.ui; }
        self.rebuild();
        self.layout();
        self.invalidate();
    }

    fn rebuild(&mut self) {
        let integration = integration_level();
        self.sections = model::build(&model::Snapshot { ui: &self.ui, integration, themed: crate::edition::THEMED });
        self.section = self.section.min(self.sections.len().saturating_sub(1));
        // A section may have lost or gained rows; keep the scroll offset valid.
        self.scroll = self.scroll.max(0.0);
    }

    fn apply_frame(&mut self) {
        let (family, _) = zsdesk_windows::gfx::system_ui_font(Some(self.hwnd));
        if self.gfx.is_none() { self.gfx = Gfx::new(&family).ok(); }
        let appearance = if crate::edition::THEMED { self.ui.appearance.clone() } else { Default::default() };
        self.dark = zsdesk_windows::theme::configure_color_scheme(appearance.color_scheme);
        zsdesk_windows::theme::set_dark_frame(self.hwnd, self.dark);
        zsdesk_windows::theme::round_corners(self.hwnd, false);
        zsdesk_windows::theme::set_backdrop(self.hwnd, appearance.backdrop);
    }

    fn palette(&self) -> Palette { Palette::for_mode(if self.dark { Mode::Dark } else { Mode::Light }) }

    fn layout(&mut self) {
        let (width, height) = self.client_size();
        let Some(gfx) = self.gfx.as_ref() else { return };
        let measure = |text: &str, size: f32, weight: u16| gfx.measure(text, &TextStyle::ui(size).weight(weight), 4096.0).0;
        self.layout = model::layout(width, height, self.dpi, &self.sections, self.section, self.scroll, &measure);
        self.scroll = self.layout.scroll;
    }

    fn client_size(&self) -> (i32, i32) {
        let mut rect = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut rect) }.is_err() { return (1, 1); }
        ((rect.right - rect.left).max(1), (rect.bottom - rect.top).max(1))
    }

    fn invalidate(&self) { let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) }; }

    fn paint(&mut self) {
        let (width, height) = self.client_size();
        let Some(gfx) = self.gfx.as_ref() else { return };
        if Surface::ensure(&mut self.surface, width, height).is_none() { return; }
        let measure = |text: &str, size: f32, weight: u16| gfx.measure(text, &TextStyle::ui(size).weight(weight), 4096.0).0;
        self.layout = model::layout(width, height, self.dpi, &self.sections, self.section, self.scroll, &measure);
        self.scroll = self.layout.scroll;
        let Some(surface) = self.surface.as_ref() else { return };
        let palette = self.palette();
        let _ = gfx.draw(surface, |canvas| {
            paint::Paint {
                canvas, layout: &self.layout, sections: &self.sections, section: self.section,
                palette, dark: self.dark, active: self.active, hover: self.hover, scroll: self.scroll,
                message: self.message.as_deref(),
            }.draw();
        });
    }

    /// Saves the preference block and lets the file-manager window refresh.
    fn commit(&mut self) {
        if let Ok(mut saved) = crate::storage::load(&self.config_path) {
            saved.ui = self.ui.clone();
            if let Err(error) = crate::storage::save(&self.config_path, &saved) {
                self.message = Some(format!("设置未保存：{error}"));
            }
        }
        self.rebuild();
        self.layout();
        unsafe { PostMessageW(Some(self.parent), WM_SETTINGS_CHANGED, WPARAM(0), LPARAM(0)).ok(); }
        self.invalidate();
    }

    /// Runs an action that belongs to the file-manager window.
    fn delegate(&mut self, action: &str) {
        let payload = Box::into_raw(Box::new(action.to_string()));
        let handled = unsafe {
            SendMessageW(self.parent, WM_SETTINGS_ACTION, Some(WPARAM(0)), Some(LPARAM(payload as isize)))
        };
        if handled.0 == 0 { unsafe { drop(Box::from_raw(payload)); } }
        self.reload();
    }

    fn run(&mut self, action: &str) {
        match action {
            "navigation-expand-system" => { self.ui.navigation_auto_expand = None; return self.commit(); }
            "navigation-expand-always" => { self.ui.navigation_auto_expand = Some(true); return self.commit(); }
            "navigation-expand-never" => { self.ui.navigation_auto_expand = Some(false); return self.commit(); }
            "appearance-solid" => { self.ui.appearance.backdrop = BackdropStyle::Solid; self.apply_frame(); return self.commit(); }
            "context-menu-defaults" => {
                self.ui.context_menu = ContextMenuPreferences::default();
                return self.commit();
            }
            _ => {}
        }
        if action.starts_with("context-menu-") || action.starts_with("context-action-") {
            let mut options = self.ui.context_menu.clone();
            if menus::apply_context_menu_setting(&mut options, action) { self.ui.context_menu = options; }
            return self.commit();
        }
        if action.starts_with("appearance-") {
            if menus::apply_appearance(&mut self.ui.appearance, action) { self.apply_frame(); return self.commit(); }
            return;
        }
        let mut ui = self.ui.clone();
        if menus::apply_interface_setting(&mut ui, action) { self.ui = ui; return self.commit(); }
        self.delegate(action);
    }

    fn choose(&mut self, target: Target) {
        match target {
            Target::Close => { let _ = unsafe { DestroyWindow(self.hwnd) }; }
            Target::Section(index) => {
                if index != self.section {
                    self.section = index; self.scroll = 0.0; self.layout(); self.invalidate();
                }
            }
            Target::Row { group, row } => {
                let Some(entry) = self.row(group, row) else { return };
                match entry.control {
                    model::Control::Toggle { enabled: false, .. } => return,
                    model::Control::Button { enabled: false } => return,
                    model::Control::Value => return,
                    _ => {}
                }
                if let Some(action) = entry.action(None) { self.run(action); }
            }
            Target::Option { group, row, option } => {
                let Some(entry) = self.row(group, row) else { return };
                if let Some(action) = entry.action(Some(option)) { self.run(action); }
            }
            Target::Scrollbar => {
                let page = self.layout.view.h * 0.85;
                self.scroll = (self.scroll + page).min(self.layout.max_scroll);
                self.layout();
                self.invalidate();
            }
            Target::None => {}
        }
    }

    fn row(&self, group: usize, row: usize) -> Option<&model::Row> {
        self.sections.get(self.section)?.groups.get(group)?.rows.get(row)
    }

    /// The header doubles as the dialog's drag handle; the close button keeps
    /// its own click area.
    fn draggable(&self, x: f32, y: f32) -> bool {
        self.layout.header.contains(x, y) && !self.layout.close.inset(-8.0, -8.0).contains(x, y)
    }

    fn scroll_by(&mut self, delta: f32) {
        let next = (self.scroll + delta).clamp(0.0, self.layout.max_scroll);
        if (next - self.scroll).abs() < 0.01 { return; }
        self.scroll = next;
        self.layout();
        self.invalidate();
    }

    fn set_hover(&mut self, target: Target) {
        if target == self.hover { return; }
        self.hover = target;
        self.invalidate();
    }

    unsafe fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = unsafe { BeginPaint(self.hwnd, &mut ps) };
                self.paint();
                if let Some(surface) = &self.surface {
                    let r = ps.rcPaint;
                    let _ = unsafe { BitBlt(dc, r.left, r.top, r.right - r.left, r.bottom - r.top, Some(surface.dc), r.left, r.top, SRCCOPY) };
                }
                let _ = unsafe { EndPaint(self.hwnd, &mut ps) };
            }
            WM_ERASEBKGND => return Some(LRESULT(1)),
            WM_MOUSEMOVE => {
                if let Some((origin, window_x, window_y)) = self.drag {
                    let mut point = POINT { x: x_lparam(lp.0), y: y_lparam(lp.0) };
                    let _ = unsafe { ClientToScreen(self.hwnd, &mut point) };
                    let _ = unsafe { SetWindowPos(self.hwnd, None, window_x + point.x - origin.x, window_y + point.y - origin.y,
                        0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
                    return Some(LRESULT(0));
                }
                if !self.tracking {
                    let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
                    self.tracking = unsafe { TrackMouseEvent(&mut track) }.is_ok();
                }
                let point = POINT { x: x_lparam(lp.0), y: y_lparam(lp.0) };
                let target = self.layout.hit(point.x as f32, point.y as f32);
                self.set_hover(target);
            }
            WM_MOUSELEAVE => { self.tracking = false; self.set_hover(Target::None); }
            WM_LBUTTONDOWN => {
                let point = POINT { x: x_lparam(lp.0), y: y_lparam(lp.0) };
                let mut window = RECT::default();
                if self.draggable(point.x as f32, point.y as f32) && unsafe { GetWindowRect(self.hwnd, &mut window) }.is_ok() {
                    let mut origin = point;
                    let _ = unsafe { ClientToScreen(self.hwnd, &mut origin) };
                    let _ = unsafe { SetCapture(self.hwnd) };
                    self.drag = Some((origin, window.left, window.top));
                    self.pressed = Target::None;
                    return Some(LRESULT(0));
                }
                self.pressed = self.layout.hit(point.x as f32, point.y as f32);
                let _ = unsafe { SetCapture(self.hwnd) };
            }
            WM_LBUTTONUP => {
                if unsafe { GetCapture() } == self.hwnd { let _ = unsafe { ReleaseCapture() }; }
                if self.drag.take().is_some() { return Some(LRESULT(0)); }
                let pressed = std::mem::replace(&mut self.pressed, Target::None);
                let point = POINT { x: x_lparam(lp.0), y: y_lparam(lp.0) };
                let target = self.layout.hit(point.x as f32, point.y as f32);
                if pressed == target && target != Target::None { self.choose(target); }
            }
            WM_CAPTURECHANGED => { self.pressed = Target::None; self.drag = None; }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) as i16) as f32;
                self.scroll_by(-delta / 120.0 * model::scale(64.0, self.dpi));
            }
            WM_KEYDOWN => {
                let key = wp.0 as u32;
                if key == VK_ESCAPE.0 as u32 {
                    let _ = unsafe { DestroyWindow(self.hwnd) };
                } else if key == VK_DOWN.0 as u32 {
                    self.scroll_by(model::scale(48.0, self.dpi));
                } else if key == VK_UP.0 as u32 {
                    self.scroll_by(-model::scale(48.0, self.dpi));
                } else if key == VK_NEXT.0 as u32 {
                    self.scroll_by(self.layout.view.h * 0.85);
                } else if key == VK_PRIOR.0 as u32 {
                    self.scroll_by(-self.layout.view.h * 0.85);
                }
            }
            WM_ACTIVATE => {
                self.active = (wp.0 & 0xffff) as u32 != WA_INACTIVE;
                self.invalidate();
            }
            WM_DPICHANGED => {
                self.dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
                let r = unsafe { &*(lp.0 as *const RECT) };
                let _ = unsafe { SetWindowPos(self.hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE) };
                self.apply_frame();
                self.layout();
                self.invalidate();
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED => {
                self.apply_frame();
                self.layout();
                self.invalidate();
            }
            WM_CLOSE => { let _ = unsafe { DestroyWindow(self.hwnd) }; }
            WM_DESTROY => {
                self.surface = None;
                self.gfx = None;
            }
            WM_NCHITTEST => return Some(LRESULT(HTCLIENT as isize)),
            _ => {}
        }
        Some(LRESULT(0))
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE { return unsafe { DefWindowProcW(hwnd, msg, wp, lp) }; }
    let mut result = LRESULT(0);
    let mut handled = false;
    DIALOG.with(|slot| {
        let Ok(mut slot) = slot.try_borrow_mut() else { return; };
        let Some(dialog) = slot.as_mut().filter(|dialog| dialog.hwnd == hwnd) else { return; };
        if let Some(value) = unsafe { dialog.handle(msg, wp, lp) } { result = value; handled = true; }
    });
    // The slot is released outside the borrow above: dropping the dialog there
    // would re-enter this thread-local while it is still mutably borrowed.
    if msg == WM_DESTROY { DIALOG.with(|slot| { if let Ok(mut slot) = slot.try_borrow_mut() { let _ = slot.take(); } }); }
    if handled { result } else { unsafe { DefWindowProcW(hwnd, msg, wp, lp) } }
}

fn x_lparam(value: isize) -> i32 { (value as i16) as i32 }
fn y_lparam(value: isize) -> i32 { ((value >> 16) as i16) as i32 }

/// 0 = 不接管, 1 = 右键菜单, 2 = 默认打开.
fn integration_level() -> u8 {
    use zsdesk_windows::integration::{self, Integration};
    match integration::current() { Integration::Off => 0, Integration::ContextMenu => 1, Integration::Default => 2 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_action_is_understood() {
        let ui = ExplorerUi::default();
        let sections = model::build(&model::Snapshot { ui: &ui, integration: 2, themed: true });
        // Local preferences apply through the shared menu helpers, everything else
        // is forwarded to the window; both paths must consume the id.
        let local = |action: &str| {
            if matches!(action, "navigation-expand-system" | "navigation-expand-always" | "navigation-expand-never" | "appearance-solid") { return true; }
            if action.starts_with("context-menu-") || action.starts_with("context-action-") {
                return menus::apply_context_menu_setting(&mut ContextMenuPreferences::default(), action);
            }
            if action.starts_with("appearance-") { return menus::apply_appearance(&mut Default::default(), action); }
            menus::apply_interface_setting(&mut ExplorerUi::default(), action)
        };
        let mut forwarded = Vec::new();
        for section in &sections {
            for group in &section.groups {
                for row in &group.rows {
                    for action in &row.actions {
                        if !local(action) { forwarded.push(*action); }
                    }
                }
            }
        }
        for action in ["new-window", "quick-search", "bookmark", "integration-off", "integration-context",
            "integration-default", "integration-recycle", "integration-forward", "integration-restore",
            "routing-settings", "search-everything-program", "search-index-settings", "search-everything-service",
            "search-rebuild-index", "quick-search-settings", "shortcuts", "folder-options", "about"] {
            assert!(forwarded.contains(&action), "{action} must reach the window");
        }
        // Nothing selected in the about section has an action.
        assert!(sections.iter().flat_map(|section| &section.groups).flat_map(|group| &group.rows)
            .filter(|row| row.id == "version").all(|row| row.actions.is_empty()));
    }
}
