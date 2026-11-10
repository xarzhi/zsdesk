//! Floating card that follows the pointer while a tab is dragged outside the
//! window, so the drop target stays visible even when the cursor leaves the
//! tab strip. Dropping the card creates a window for that tab.
//!
//! The card is a layered popup: a rounded window region supplies the corners,
//! `LWA_ALPHA` supplies the translucency, and the surface is painted with the
//! same Direct2D tokens as the rest of the chrome. Its window procedure only
//! blits the surface prepared by [`TabGhost::present`], so painting never
//! re-enters the file-manager window state.

use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::{COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
    Graphics::Gdi::{BeginPaint, BitBlt, CreateRoundRectRgn, EndPaint, GetMonitorInfoW, HDC, InvalidateRect,
        MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT, SetWindowRgn, SRCCOPY},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::*,
};
use zsdesk_ui::theme::{glyph, Color as ThemeColor, Palette};
use zsdesk_windows::gfx::{Color, Gfx, RectF, Surface, TextStyle};

const CLASS: PCWSTR = windows::core::w!("ZSDesk.Native.TabGhost");
/// Memory DC of the painted surface, so `WM_PAINT` can blit it back.
const SURFACE_DC: PCWSTR = windows::core::w!("ZSDesk.GhostSurface");
/// Card height in DIP: a title line and the drop hint underneath.
const HEIGHT: f32 = 44.0;
const MIN_WIDTH: f32 = 176.0;
const MAX_WIDTH: f32 = 288.0;
/// The pointer sits this far inside the card instead of on its corner.
const GRIP: (f32, f32) = (26.0, 14.0);

pub(super) struct TabGhost {
    hwnd: HWND,
    gfx: Option<Gfx>,
    surface: Option<Surface>,
    dpi: u32,
    dark: bool,
    palette: Palette,
    size: (i32, i32),
    title: String,
    hint: &'static str,
    shown: bool,
}

impl TabGhost {
    /// Creates the (hidden) card. Reuses one window per file-manager window.
    pub(super) fn new(owner: HWND, dpi: u32) -> Option<Self> {
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None).ok()?.0 });
        unsafe {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
                lpszClassName: CLASS,
                ..Default::default()
            });
        }
        let (family, _) = zsdesk_windows::gfx::system_ui_font(Some(owner));
        let gfx = Gfx::new(&family).ok()?;
        let hwnd = unsafe {
            CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                CLASS, PCWSTR(wide("").as_ptr()), WS_POPUP, 0, 0, 10, 10, Some(owner), None, Some(instance), None).ok()?
        };
        unsafe { let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 246, LWA_ALPHA); }
        Some(Self {
            hwnd, gfx: Some(gfx), surface: None, dpi, dark: false, palette: Palette::light(),
            size: (10, 10), title: String::new(), hint: "", shown: false,
        })
    }

    /// Shows the card under `screen` (a screen point) with `title` on it.
    pub(super) fn present(&mut self, screen: POINT, title: &str, hint: &'static str, dark: bool, palette: Palette) {
        let restyled = self.title != title || self.hint != hint || self.dark != dark;
        if restyled {
            self.title.clear();
            self.title.push_str(title);
            self.hint = hint;
        }
        self.dark = dark;
        self.palette = palette;
        let size = self.measure();
        let resized = size != self.size;
        self.size = size;
        if restyled || resized || !self.shown { self.paint(); }
        let (width, height) = self.size;
        let (x, y) = grip_origin(screen, self.size, self.dpi);
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, width, height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW);
            if resized || !self.shown {
                let radius = self.scale(18.0) as i32;
                let region = CreateRoundRectRgn(0, 0, width + 1, height + 1, radius, radius);
                // `SetWindowRgn` owns the region once it succeeds.
                let _ = SetWindowRgn(self.hwnd, Some(region), true);
            }
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        self.shown = true;
    }

    pub(super) fn hide(&mut self) {
        if !self.shown { return; }
        self.shown = false;
        unsafe { let _ = ShowWindow(self.hwnd, SW_HIDE); }
    }

    fn scale(&self, value: f32) -> f32 { value * self.dpi as f32 / 96.0 }

    /// Card width follows the longer of the two lines but stays in a readable
    /// range.
    fn measure(&self) -> (i32, i32) {
        let Some(gfx) = self.gfx.as_ref() else { return self.size };
        let (title, _) = gfx.measure(&self.title, &TextStyle::ui(self.scale(12.5)).weight(600), 4096.0);
        let (hint, _) = gfx.measure(self.hint, &TextStyle::ui(self.scale(10.5)), 4096.0);
        let width = (title.max(hint) + self.scale(56.0)).clamp(self.scale(MIN_WIDTH), self.scale(MAX_WIDTH));
        (width.round() as i32, self.scale(HEIGHT).round() as i32)
    }

    fn paint(&mut self) {
        let (width, height) = self.size;
        let Some(gfx) = self.gfx.as_ref() else { return };
        if Surface::ensure(&mut self.surface, width, height).is_none() { return; }
        let Some(surface) = self.surface.as_ref() else { return };
        let palette = self.palette;
        let s = |value: f32| value * self.dpi as f32 / 96.0;
        // A drop shadow is not available without a compositor surface, so the
        // card is drawn with a solid body and a hairline border instead.
        let body = if self.dark { Color::rgba8(0x2B, 0x2B, 0x2B, 0xFF) } else { Color::rgba8(0xFB, 0xFB, 0xFB, 0xFF) };
        let title = self.title.clone();
        let hint = self.hint;
        let _ = gfx.draw(surface, |cv| {
            let card = RectF::new(0.0, 0.0, width as f32, height as f32);
            cv.fill(card, body);
            let border = RectF::new(0.5, 0.5, width as f32 - 1.0, height as f32 - 1.0);
            cv.stroke_round(border, s(9.0), rgba(palette.stroke), 1.0);
            let icon = RectF::new(s(14.0), s(11.0), s(15.0), s(15.0));
            cv.glyph(glyph::FOLDER, icon, s(14.0), rgba(palette.text));
            cv.text(&title, RectF::new(icon.right() + s(8.0), s(8.0), (width as f32 - icon.right() - s(26.0)).max(0.0), s(18.0)),
                &TextStyle::ui(s(12.5)).weight(600), rgba(palette.text));
            cv.text(hint, RectF::new(s(14.0), s(26.0), (width as f32 - s(24.0)).max(0.0), s(14.0)),
                &TextStyle::ui(s(10.5)), rgba(palette.text_secondary));
        });
        unsafe { let _ = SetPropW(self.hwnd, SURFACE_DC, Some(HANDLE(surface.dc.0))); }
    }
}

impl Drop for TabGhost {
    fn drop(&mut self) {
        unsafe {
            let _ = RemovePropW(self.hwnd, SURFACE_DC);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Screen origin that keeps `GRIP` of the card under the pointer and the whole
/// card on the monitor the pointer is over.
fn grip_origin(point: POINT, size: (i32, i32), dpi: u32) -> (i32, i32) {
    let scale = |value: f32| (value * dpi as f32 / 96.0).round() as i32;
    let left = point.x - scale(GRIP.0);
    let top = point.y - scale(GRIP.1);
    let mut area = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    let work = if unsafe { GetMonitorInfoW(monitor, &mut area) }.as_bool() {
        area.rcWork
    } else { windows::Win32::Foundation::RECT { left: 0, top: 0, right: i32::MAX, bottom: i32::MAX } };
    (
        left.clamp(work.left, (work.right - size.0).max(work.left)),
        top.clamp(work.top, (work.bottom - size.1).max(work.top)),
    )
}

fn rgba(color: ThemeColor) -> Color { Color::rgba8(color.r, color.g, color.b, color.a) }

fn wide(text: &str) -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() }

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match message {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            let source = unsafe { GetPropW(hwnd, SURFACE_DC) };
            if !source.0.is_null() {
                let mut client = windows::Win32::Foundation::RECT::default();
                let _ = unsafe { GetClientRect(hwnd, &mut client) };
                let _ = unsafe { BitBlt(dc, 0, 0, client.right, client.bottom, Some(HDC(source.0)), 0, 0, SRCCOPY) };
            }
            let _ = unsafe { EndPaint(hwnd, &paint) };
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wp, lp) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_hangs_below_and_right_of_the_pointer() {
        // 96 DPI: the grip offset is applied unscaled.
        let (x, y) = grip_origin(POINT { x: 800, y: 500 }, (200, 44), 96);
        assert_eq!((x, y), (800 - 26, 500 - 14));
        // 192 DPI doubles it, matching the DIP sizes of the card itself.
        let (x, y) = grip_origin(POINT { x: 800, y: 500 }, (400, 88), 192);
        assert_eq!((x, y), (800 - 52, 500 - 28));
    }
}
