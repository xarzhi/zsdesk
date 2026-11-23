//! Self-drawn text for the native EDIT fields.
//!
//! A DWM backdrop extended over the whole client area makes the window composite
//! by pixel alpha. GDI — which every EDIT control uses — writes colour without
//! alpha, so glyphs drawn by a native text field end up transparent and read as
//! pale coloured ghosts in front of the material.
//!
//! The EDIT stays the input model: it keeps its position, focus, keyboard, IME,
//! clipboard and undo behaviour. It is made hollow instead — an empty window
//! region plus a subclass that validates its paints — so it puts no pixels on
//! screen, and the text, selection, caret and IME composition are drawn here
//! with Direct2D, on top of the backdrop that the window itself painted.

use super::*;
use zsdesk_windows::gfx::{Canvas, Color, RectF, TextStyle};

/// Width given to DirectWrite when measuring a single line: large enough for
/// any field, small enough to keep its layout arithmetic exact.
const MAX_MEASURE: f32 = 8192.0;

/// Caret blink interval in milliseconds, close to the system default.
pub(super) const CARET_INTERVAL: u32 = 530;
/// Timer that blinks the self-drawn caret. Kept apart from the other timers:
/// sharing an id with the navigation refresh made one of them stop the other.
pub(super) const TIMER_CARET: usize = 13;

/// Marks a text field whose pixels must not reach the screen: the control keeps
/// its position, focus, keyboard, IME and clipboard, but its subclass validates
/// every paint instead of drawing one, and `paint_edits` draws the text.
pub(super) const HOLLOW_FIELD: windows::core::PCWSTR = windows::core::w!("ZSDesk.HollowField");

/// Window size a hollow field keeps while a material is on.
///
/// The subclass stops paints, but it cannot stop the compositor from showing
/// whatever the control already has in its backing store: a click moves focus
/// and makes the system recompose the control, and the white plate an EDIT
/// erased into that store before it was hollowed out flashes once again. GDI
/// clips that erase by the window rectangle (measured: the slab matched the
/// control's rectangle exactly, 236×17 px), so a one-pixel control can only
/// ever mark one pixel. Focus, keyboard, IME, clipboard and undo all still
/// work, and the IME's candidate window still opens at the text origin.
const HOLLOW_SIZE: i32 = 1;

/// True when the field must stay invisible. A null handle has nothing to paint.
pub(super) fn field_hollow(hwnd: HWND) -> bool {
    hwnd.0.is_null() || unsafe { !GetPropW(hwnd, HOLLOW_FIELD).0.is_null() }
}

/// Everything one visible text field needs at paint time. Collected before the
/// drawing closure runs, because measuring and scrolling need `&mut self`.
pub(super) struct EditPaint {
    rect: RectF,
    text: String,
    cue: String,
    composition: String,
    selection: (usize, usize),
    /// Width of the text before the selection start / end, caret and composition.
    sel_from: f32,
    sel_to: f32,
    caret: f32,
    composition_width: f32,
    text_width: f32,
    scroll: f32,
    focused: bool,
    /// Draws the text with the same size the row uses (the rename field sits
    /// on a painted name), and centres it when the row centres its label.
    style: TextStyle,
    center: bool,
    /// Draws a real field behind the text. The in-place rename editor covers a
    /// painted name, and text floating on the wallpaper reads as neither.
    field_box: bool,
}

impl WindowState {
    /// Text fields this window owns.
    fn edit_fields(&self) -> [HWND; 4] {
        [self.search_edit, self.address_edit, self.view_rename, self.rename_hwnd]
    }

    /// Text area of `edit`, as the layout last placed it. The control itself is
    /// shrunk while a material is on, so its window rectangle is not the answer.
    fn edit_area(&self, edit: HWND) -> Option<LayoutRect> {
        let area = if edit == self.search_edit { self.search_area }
            else if edit == self.address_edit { self.address_area }
            else if edit == self.view_rename { self.view_rename_area }
            else if !self.rename_hwnd.0.is_null() && edit == self.rename_hwnd { self.rename_area }
            else { return None };
        if area.is_empty() { None } else { Some(area) }
    }

    /// Records where a field's text belongs and places the control there.
    ///
    /// Under a material the control is then squeezed to a single pixel: it keeps
    /// its position, focus, keyboard, IME context and clipboard, but the plate
    /// it would erase there is clipped to that pixel, so it cannot paint the
    /// white slab over the translucent field; the text is drawn by
    /// `paint_edits` instead.
    pub(super) fn place_edit(&mut self, edit: HWND, area: LayoutRect) {
        if edit.0.is_null() { return; }
        if edit == self.search_edit { self.search_area = area; }
        else if edit == self.address_edit { self.address_area = area; }
        else if edit == self.view_rename { self.view_rename_area = area; }
        else if edit == self.rename_hwnd { self.rename_area = area; }
        else { return; }
        self.place_control(edit, area);
        self.sync_edit_regions();
    }

    /// Moves a field's control to where its text lives.
    ///
    /// A hollow field is pinned to one pixel at the text origin, and the move
    /// does not redraw: the control has nothing to show, and letting it repaint
    /// on every layout is what puts its white backing store back on screen.
    fn place_control(&self, edit: HWND, area: LayoutRect) {
        if self.material {
            unsafe { move_control_quiet(edit, LayoutRect::new(area.x, area.y, HOLLOW_SIZE, HOLLOW_SIZE)); }
        } else {
            unsafe { move_control(edit, area); }
        }
    }

    /// Applies the material rule to every text field: hollowed out and squeezed
    /// to a pixel when DWM composites the window by pixel alpha, restored to its
    /// real size and colours when it does not.
    ///
    /// Hollowing takes three things, because none alone is enough. An empty
    /// window region makes the control unhittable — clicks reach the chrome
    /// instead of a native field — but it does *not* stop the control from
    /// painting: GDI erases an EDIT's background without consulting that region
    /// (measured: the window region is empty while the DC clip box still covers
    /// the whole field). The hollow mark makes the subclass validate paints
    /// instead of drawing them, which still leaves the white already in the
    /// control's backing store, and the compositor shows it again every time
    /// focus or layout makes it recompose the control. So the control is also
    /// squeezed to one pixel: an erase clipped to one pixel is invisible, and a
    /// transition repaints the chrome once to clear what was on screen.
    pub(super) fn sync_edit_regions(&mut self) {
        let changed = self.material != self.edits_hollow;
        for edit in self.edit_fields() {
            if edit.0.is_null() { continue; }
            let area = self.edit_area(edit);
            if self.material {
                // The state is decided per control, not by `edits_hollow`: the
                // rename editors are created lazily, long after the window
                // switched to a material, so the window's own flag says nothing
                // about them. A field made later kept its real class behaviour —
                // it painted and it kept the system caret, which showed up as a
                // second, blinking bar at the field's origin.
                if field_hollow(edit) { continue; }
                // The mark goes on first: the squeeze that follows must not make
                // the control paint the one pixel it keeps.
                unsafe { let _ = SetPropW(edit, HOLLOW_FIELD, Some(windows::Win32::Foundation::HANDLE(1usize as *mut _))); }
                if let Some(area) = area { self.place_control(edit, area); }
                unsafe {
                    let region = CreateRectRgn(0, 0, 0, 0);
                    if !region.is_invalid() {
                        // The window owns the region only when the call succeeds.
                        if SetWindowRgn(edit, Some(region), true) == 0 { let _ = DeleteObject(region.into()); }
                    }
                }
            } else if let Some(area) = area {
                unsafe {
                    let _ = RemovePropW(edit, HOLLOW_FIELD);
                    let _ = SetWindowRgn(edit, None, true);
                    move_control(edit, area);
                }
            }
        }
        self.edits_hollow = self.material;
        if changed && self.material { self.invalidate(); }
    }

    /// Rectangle a field's text is drawn into, in canvas pixels.
    fn edit_rect(&self, edit: HWND) -> Option<RectF> {
        let area = self.edit_area(edit)?;
        Some(RectF::new(area.x as f32, area.y as f32, area.width as f32, area.height as f32))
    }

    /// The control a `WM_COMMAND` notification id belongs to.
    pub(super) fn edit_handle(&self, id: usize) -> Option<HWND> {
        let edit = if id == ID_SEARCH { self.search_edit }
            else if id == ID_ADDRESS { self.address_edit }
            else if id == ID_VIEW_RENAME { self.view_rename }
            else if id == ID_BOOKMARK_RENAME { self.rename_hwnd }
            else { return None };
        (!edit.0.is_null()).then_some(edit)
    }

    /// Repaints a field after the user typed, deleted or moved the caret.
    ///
    /// A hollow field paints nothing itself, so without this nothing on screen
    /// changes until something else — the caret blink or Enter — happens to
    /// repaint the window. That is what made in-place rename look dead while
    /// typing and only take effect after Enter.
    pub(super) fn refresh_edit(&mut self, edit: HWND) {
        if edit.0.is_null() { return; }
        self.caret_on = true;
        match self.edit_area(edit) {
            Some(area) => self.invalidate_rect(area),
            None => self.invalidate(),
        }
    }

    pub(super) fn visible_edit_at(&self, p: POINT) -> Option<HWND> {
        if !self.material { return None; }
        for edit in self.edit_fields() {
            if edit.0.is_null() || !unsafe { IsWindowVisible(edit) }.as_bool() { continue; }
            if self.edit_rect(edit).is_some_and(|rect| rect.contains(p.x as f32, p.y as f32)) { return Some(edit); }
        }
        None
    }

    /// A click landed on a self-drawn field: place the caret, focus the control
    /// and remember it for a drag selection.
    pub(super) fn focus_edit_at(&mut self, p: POINT, shift: bool) -> bool {
        let Some(edit) = self.visible_edit_at(p) else { return false };
        let index = self.edit_index_at(edit, p);
        let (start, _) = edit_selection(edit);
        let anchor = if shift { start } else { index };
        let _ = unsafe { SetFocus(Some(edit)) };
        unsafe { SendMessageW(edit, EM_SETSEL, Some(WPARAM(anchor)), Some(LPARAM(index as isize))); }
        self.edit_drag = Some(edit.0 as isize);
        self.caret_on = true;
        let _ = unsafe { SetCapture(self.hwnd) };
        self.invalidate();
        true
    }

    /// Extends the selection to the pointer while a field is being dragged.
    pub(super) fn drag_edit_selection(&mut self, p: POINT) -> bool {
        let Some(handle) = self.edit_drag else { return false };
        let edit = HWND(handle as *mut _);
        let index = self.edit_index_at(edit, p);
        let (start, _) = edit_selection(edit);
        unsafe { SendMessageW(edit, EM_SETSEL, Some(WPARAM(start)), Some(LPARAM(index as isize))); }
        self.invalidate();
        true
    }

    pub(super) fn end_edit_drag(&mut self) -> bool {
        self.edit_drag.take().is_some()
    }

    /// Double click inside a field selects the whole value, like an EDIT does.
    pub(super) fn select_edit_at(&mut self, p: POINT) -> bool {
        let Some(edit) = self.visible_edit_at(p) else { return false };
        let _ = unsafe { SetFocus(Some(edit)) };
        unsafe { SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1))); }
        self.caret_on = true;
        self.invalidate();
        true
    }

    /// Character index under a client point, measured with the same DirectWrite
    /// style the text is drawn with. The EDIT itself cannot answer this: it was
    /// shrunk to a pixel and scrolled its own (invisible) text to keep the caret
    /// in that pixel, so `EM_CHARFROMPOS` would be off by the scroll amount.
    /// True when the field's text is drawn centred inside its box, because the
    /// row it covers centres its label.
    fn centers_edit(&self, edit: HWND) -> bool {
        edit == self.view_rename && self.view_rename_index.is_some() && self.file_view.centers_names()
    }

    fn edit_index_at(&self, edit: HWND, p: POINT) -> usize {
        let Some(area) = self.edit_area(edit) else { return 0 };
        let text = edit_text(edit);
        let style = self.field_style(edit);
        let width = area.width as f32;
        let origin = area.x as f32 + if self.centers_edit(edit) {
            ((width - self.text_width(&text, &style)) / 2.0).max(0.0)
        } else { 0.0 };
        self.index_at(&text, &style, p.x as f32 - origin)
    }

    /// Character index closest to `x` pixels into `text`.
    fn index_at(&self, text: &str, style: &TextStyle, x: f32) -> usize {
        if x <= 0.0 { return 0; }
        let mut bounds = Vec::with_capacity(8);
        let mut units = 0;
        for character in text.chars() { bounds.push(units); units += character.len_utf16(); }
        bounds.push(units);
        let (mut low, mut high) = (0, bounds.len() - 1);
        while low < high {
            let mid = (low + high + 1) / 2;
            if self.text_width(prefix_units(text, bounds[mid]), style) <= x { low = mid } else { high = mid - 1; }
        }
        // Snap to whichever side of the glyph the pointer is closer to.
        if low + 1 < bounds.len() {
            let here = self.text_width(prefix_units(text, bounds[low]), style);
            let next = self.text_width(prefix_units(text, bounds[low + 1]), style);
            if next - x < x - here { return bounds[low + 1]; }
        }
        bounds[low]
    }

    fn any_edit_focused(&self) -> bool {
        edit_fields_iter(self).any(|edit| unsafe { GetFocus() } == edit)
    }

    /// Starts or stops the blink of the self-drawn caret.
    ///
    /// Driven from a posted message as well as from `EN_SETFOCUS`, because the
    /// in-place rename editor is focused from inside an action: the window
    /// state is still borrowed then, so the EDIT's `WM_COMMAND` notification
    /// never reaches the handler and the caret stayed frozen — visible next to
    /// the native caret the system drew at the field's own origin.
    pub(super) fn edit_focus_changed(&mut self, focused: bool) {
        self.caret_on = focused;
        if focused {
            unsafe { SetTimer(Some(self.hwnd), TIMER_CARET, CARET_INTERVAL, None); }
        } else {
            let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_CARET) };
        }
        self.invalidate();
    }

    /// Blinks the caret while one of the fields has the focus.
    pub(super) fn tick_caret(&mut self) {
        if !self.any_edit_focused() {
            self.caret_timer = false;
            let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_CARET) };
            return;
        }
        self.caret_on = !self.caret_on;
        self.invalidate();
    }

    /// Collects the text of every visible field before drawing.
    pub(super) fn prepare_edits(&mut self) -> Vec<EditPaint> {
        if !self.material { return Vec::new(); }
        let mut paints = Vec::new();
        let mut focused_any = false;
        for edit in self.edit_fields() {
            if edit.0.is_null() || !unsafe { IsWindowVisible(edit) }.as_bool() { continue; }
            let Some(rect) = self.edit_rect(edit) else { continue };
            let text = edit_text(edit);
            let selection = edit_selection(edit);
            let focused = unsafe { GetFocus() } == edit;
            focused_any |= focused;
            let style = self.field_style(edit);
            // A centred label (every icon layout) is drawn from a centred
            // origin, so the field must not scroll: the text stays inside the
            // cell the way the painted name does.
            let center = self.centers_edit(edit);
            let scroll = if center { 0.0 } else { self.edit_scroll(edit, &text, &style, selection.1.max(selection.0), rect, focused) };
            let caret = self.text_width(prefix_units(&text, selection.1.max(selection.0)), &style);
            let composition = if focused { edit_composition(edit) } else { String::new() };
            paints.push(EditPaint {
                rect, text_width: self.text_width(&text, &style), style, center,
                // The two in-place editors cover a painted label and need a
                // surface of their own; the chrome fields have their own box.
                field_box: edit == self.view_rename
                    || (!self.rename_hwnd.0.is_null() && edit == self.rename_hwnd),
                cue: if text.is_empty() && edit == self.search_edit { self.search_cue.clone() } else { String::new() },
                sel_from: self.text_width(prefix_units(&text, selection.0), &style),
                sel_to: self.text_width(prefix_units(&text, selection.1), &style),
                composition_width: self.text_width(&composition, &style),
                composition, selection, caret, scroll, focused,
                text,
            });
        }
        self.caret_timer = focused_any;
        paints
    }

    /// Draws the collected fields. Runs inside the paint closure, so it only
    /// reads state.
    pub(super) fn paint_edits(&self, cv: &Canvas, edits: &[EditPaint]) {
        if edits.is_empty() { return; }
        let cue_color = paint::c(self.palette.text_secondary).with_alpha(0.8);
        let caret_width = self.sf(1.0).max(1.0);
        for edit in edits {
            let rect = edit.rect;
            // Left edge of the text inside the field: a centred field shifts it
            // so the value sits where the row paints its label.
            let shift = rect.x + if edit.center { ((rect.w - edit.text_width) / 2.0).max(0.0) } else { 0.0 } - edit.scroll;
            // A rename editor is a white box, the way Explorer opens one over a
            // name: a solid surface hides the row underneath, and the value is
            // legible over a wallpaper in either theme. Its text is therefore
            // dark whatever colour the rest of the window uses.
            let text_color = if edit.field_box { Color::rgba8(0x1B, 0x1B, 0x1B, 0xFF) } else { paint::c(self.palette.text) };
            if edit.field_box {
                let field = RectF::new(rect.x - self.sf(3.0), rect.y - self.sf(1.0),
                    rect.w + self.sf(6.0), rect.h + self.sf(2.0));
                cv.round(field, self.sf(2.0), Color::rgba8(0xFF, 0xFF, 0xFF, 0xFF));
                cv.stroke_round(field, self.sf(2.0), paint::c(self.palette.accent), self.sf(1.0));
            }
            cv.push_clip(rect);
            if edit.text.is_empty() {
                if !edit.cue.is_empty() { cv.text(&edit.cue, rect, &edit.style, cue_color); }
            } else {
                if edit.selection.1 > edit.selection.0 {
                    cv.fill(RectF::new(shift + edit.sel_from, rect.y,
                        (edit.sel_to - edit.sel_from).max(1.0), rect.h), paint::c(self.palette.accent).with_alpha(if edit.field_box { 0.28 } else { 0.32 }));
                }
                cv.text(&edit.text, RectF::new(shift, rect.y, edit.text_width + caret_width * 2.0, rect.h),
                    &edit.style, text_color);
                if !edit.composition.is_empty() {
                    cv.text(&edit.composition, RectF::new(shift + edit.caret, rect.y,
                        edit.composition_width + caret_width * 2.0, rect.h), &edit.style, text_color);
                    cv.line(shift + edit.caret, rect.bottom() - self.sf(1.5),
                        shift + edit.caret + edit.composition_width,
                        rect.bottom() - self.sf(1.5), text_color, caret_width);
                }
            }
            // The caret sits on the placeholder too: an empty focused field has
            // to show where typing will land, exactly like the native control.
            if edit.focused && self.caret_on && edit.composition.is_empty() {
                let caret_x = shift + edit.caret;
                cv.fill(RectF::new(caret_x, rect.y + self.sf(1.0), caret_width, rect.h - self.sf(2.0)), text_color);
            }
            cv.pop_clip();
        }
    }

    pub(super) fn text_style(&self) -> TextStyle {
        let mut style = self.ui_style();
        style.ellipsis = false;
        style.wrap = false;
        style
    }

    /// Style one field's text is drawn with.
    ///
    /// The rename editor covers a painted name, so it has to use the size the
    /// row paints with; the search and address fields keep the chrome size.
    fn field_style(&self, edit: HWND) -> TextStyle {
        if edit == self.view_rename {
            let mut style = TextStyle::ui(self.sf(12.5));
            style.ellipsis = false;
            style.wrap = false;
            return style;
        }
        self.text_style()
    }

    /// Horizontal offset that keeps the caret inside the field, kept per control
    /// so the text does not jump while the caret blinks.
    fn edit_scroll(&mut self, edit: HWND, text: &str, style: &TextStyle, caret: usize, rect: RectF, focused: bool) -> f32 {
        let key = edit.0 as isize;
        let caret_x = self.text_width(prefix_units(text, caret), style);
        let mut scroll = self.edit_scroll.get(&key).copied().unwrap_or(0.0);
        if !focused {
            scroll = 0.0;
        } else {
            let margin = self.sf(2.0);
            if caret_x - scroll > rect.w - margin { scroll = caret_x - rect.w + margin; }
            if caret_x - scroll < 0.0 { scroll = caret_x; }
            if scroll < 0.0 { scroll = 0.0; }
        }
        self.edit_scroll.insert(key, scroll);
        scroll
    }

    fn text_width(&self, text: &str, style: &TextStyle) -> f32 {
        match self.gfx.as_ref() {
            Some(gfx) if !text.is_empty() => gfx.measure(text, style, MAX_MEASURE).0,
            _ => 0.0,
        }
    }
}

fn edit_fields_iter(state: &WindowState) -> impl Iterator<Item = HWND> + '_ {
    [state.search_edit, state.address_edit, state.view_rename, state.rename_hwnd].into_iter()
}

fn edit_text(edit: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(edit) };
    if length <= 0 { return String::new(); }
    let mut buffer = vec![0u16; length as usize + 1];
    let copied = unsafe { GetWindowTextW(edit, &mut buffer) };
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

/// Selection in UTF-16 units, as the EDIT reports it.
fn edit_selection(edit: HWND) -> (usize, usize) {
    let packed = unsafe { SendMessageW(edit, EM_GETSEL, None, None) }.0 as u32;
    ((packed & 0xffff) as usize, ((packed >> 16) & 0xffff) as usize)
}


/// The IME composition string, empty when the input method is not composing.
fn edit_composition(edit: HWND) -> String {
    use windows::Win32::UI::Input::Ime::{ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, GCS_COMPSTR};
    let context = unsafe { ImmGetContext(edit) };
    if context.0.is_null() { return String::new(); }
    let length = unsafe { ImmGetCompositionStringW(context, GCS_COMPSTR, None, 0) };
    let mut result = String::new();
    if length > 0 {
        let mut bytes = vec![0u8; length as usize];
        let copied = unsafe {
            ImmGetCompositionStringW(context, GCS_COMPSTR, Some(bytes.as_mut_ptr().cast()), length as u32)
        };
        if copied > 0 {
            let units: Vec<u16> = bytes[..copied as usize].chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
            result = String::from_utf16_lossy(&units);
        }
    }
    let _ = unsafe { ImmReleaseContext(edit, context) };
    result
}

/// Byte prefix that covers `units` UTF-16 code units.
fn prefix_units(text: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, character) in text.char_indices() {
        if count >= units { return &text[..index]; }
        count += character.len_utf16();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_units_respects_surrogate_pairs() {
        assert_eq!(prefix_units("abc", 2), "ab");
        assert_eq!(prefix_units("a中b", 2), "a中");
        assert_eq!(prefix_units("😀x", 2), "😀");
        assert_eq!(prefix_units("abc", 9), "abc");
    }

    /// A material must leave the field unable to paint at all — an empty window
    /// region alone still let an EDIT erase its own background onto the screen.
    #[test]
    fn a_material_marks_the_text_fields_hollow_and_classic_undoes_it() {
        unsafe {
            let parent = CreateWindowExW(WINDOW_EX_STYLE::default(), windows::core::w!("STATIC"), None,
                WS_OVERLAPPED, 0, 0, 320, 80, None, None, None, None).expect("parent window");
            let edit = child(parent, "EDIT", "", ID_SEARCH, ES_AUTOHSCROLL as u32, true).expect("search field");
            let mut state = WindowState::new(storage::default_session(),
                std::env::temp_dir().join("zsdesk-hollow-field.json"), None, false);
            state.hwnd = parent;
            state.search_edit = edit;
            state.material = true;
            state.place_edit(edit, LayoutRect::new(10, 10, 200, 19));
            assert!(field_hollow(edit), "a field under a material must not paint");
            let region = CreateRectRgn(0, 0, 0, 0);
            let mut box_ = RECT::default();
            assert_ne!(windows::Win32::Graphics::Gdi::GetWindowRgn(edit, region).0, 0, "the field keeps a window region");
            let _ = windows::Win32::Graphics::Gdi::GetRgnBox(region, &mut box_);
            assert_eq!((box_.right - box_.left, box_.bottom - box_.top), (0, 0), "the region is empty");
            // An erase is clipped by the control's rectangle, not by its region,
            // so the control itself has to shrink: a click re-composes it and
            // showed the white it erased before it was hollowed out.
            let mut client = RECT::default();
            let _ = GetClientRect(edit, &mut client);
            assert_eq!((client.right - client.left, client.bottom - client.top), (1, 1),
                "a hollow field is squeezed to one pixel");
            state.material = false;
            state.sync_edit_regions();
            assert!(!field_hollow(edit), "a classic field paints itself again");
            assert_eq!(windows::Win32::Graphics::Gdi::GetWindowRgn(edit, region).0, 0, "the empty region is gone");
            let mut client = RECT::default();
            let _ = GetClientRect(edit, &mut client);
            assert_eq!((client.right - client.left, client.bottom - client.top), (200, 19),
                "a classic field gets its text area back");
            let _ = DeleteObject(region.into());
            let _ = DestroyWindow(edit);
            let _ = DestroyWindow(parent);
        }
    }

    /// A field created after the window went translucent has to be hollowed
    /// out too. The in-place rename editors are created on first use, and they
    /// kept their class behaviour: the control painted and kept the system
    /// caret, which the user saw as a second caret bar at the field's origin.
    #[test]
    fn a_field_created_after_the_material_is_hollowed_out_as_well() {
        unsafe {
            let parent = CreateWindowExW(WINDOW_EX_STYLE::default(), windows::core::w!("STATIC"), None,
                WS_OVERLAPPED, 0, 0, 320, 80, None, None, None, None).expect("parent window");
            let search = child(parent, "EDIT", "", ID_SEARCH, ES_AUTOHSCROLL as u32, true).expect("search field");
            let mut state = WindowState::new(storage::default_session(),
                std::env::temp_dir().join("zsdesk-lazy-hollow.json"), None, false);
            state.hwnd = parent;
            state.search_edit = search;
            state.material = true;
            state.place_edit(search, LayoutRect::new(10, 10, 200, 19));
            assert!(field_hollow(search));
            // The rename editor appears only when a rename starts.
            let rename = child(parent, "EDIT", "", ID_VIEW_RENAME, ES_AUTOHSCROLL as u32, true).expect("rename field");
            assert!(!field_hollow(rename), "a brand new field starts as a normal control");
            state.view_rename = rename;
            state.place_edit(rename, LayoutRect::new(40, 40, 160, 17));
            assert!(field_hollow(rename), "placing it under the material hollows it out");
            let mut client = RECT::default();
            let _ = GetClientRect(rename, &mut client);
            assert_eq!((client.right - client.left, client.bottom - client.top), (1, 1));
            let _ = DestroyWindow(rename);
            let _ = DestroyWindow(search);
            let _ = DestroyWindow(parent);
        }
    }
}
