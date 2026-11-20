//! Wiring between the window and the self-drawn file list.
//!
//! The Shell view stays alive but hidden: it keeps the folder model, the
//! selection that every command reads, and the drag/drop targets. This module
//! only translates window input into list operations and mirrors the resulting
//! selection back, so copy, cut, delete, rename, preview and the system context
//! menu keep working unchanged.

use super::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use zsdesk_windows::{gfx::RectF, shell::folder_items};

impl WindowState {
    /// Rectangle of the file pane in client pixels.
    pub(super) fn view_rect(&self) -> RectF {
        let area = self.layout.browser;
        RectF::new(area.x as f32, area.y as f32, area.width as f32, area.height as f32)
    }

    /// Location shown by the self-drawn list, when it is in charge. Search
    /// results stay on the Shell view: their folder is a virtual one that only
    /// answers through the Shell's own item array.
    fn custom_view_location(&self) -> Option<ShellLocation> {
        if !self.session.ui.custom_view { return None; }
        match self.current_location() {
            ShellLocation::Search { .. } => None,
            location => Some(location.clone()),
        }
    }

    /// Loads the current folder and shows or hides the Shell view to match.
    pub(super) fn sync_file_view(&mut self) {
        let enabled = self.custom_view_location().is_some();
        // The status bar offers the two view-mode buttons only while the Shell
        // view paints the rows; switching between the two modes re-lays the bar.
        if self.custom_view != enabled {
            self.custom_view = enabled;
            self.relayout();
            self.position_children();
        }
        // Row indices change with the folder, so a stale click anchor must not
        // open whatever lands on that index next.
        self.last_click = None;
        // The navigation tree is drawn by ZSDesk whenever the self-drawn mode
        // is on, even while a search puts the list back on the Shell view.
        let custom_tree = self.session.ui.custom_view;
        if let Some(tree) = &self.tree { tree.set_visible(!custom_tree); }
        if custom_tree {
            let tree = self.tree_rect();
            self.nav_tree.set_viewport(tree.w.max(0.0) as i32, tree.h.max(0.0) as i32, self.dpi);
            let location = self.current_location().clone();
            self.nav_tree.reveal(&location);
        }
        // Every navigation builds a fresh Shell view that shows itself: keep it
        // hidden on each sync, not only when the mode changes.
        if let Some(browser) = &self.browser { browser.set_visible(!enabled); }
        if !enabled { return; }
        self.file_view.set_viewport(self.view_rect(), self.dpi);
        self.start_file_list();
        self.mirror_selection(&[]);
    }

    /// Starts a background listing of the folder on screen. The pane shows
    /// "正在读取…" until the result arrives, so a large folder or a slow share
    /// never freezes the window.
    fn start_file_list(&mut self) {
        let location = self.current_location().clone();
        let token = self.file_view.begin_load(&location);
        self.list_loader.start(token, location);
    }

    /// Applies every listing that finished on a worker thread.
    pub(super) fn apply_listed_folders(&mut self) {
        let mut applied = false;
        for listing in self.list_loader.drain() {
            applied |= self.file_view.finish_load(listing.token, listing.items);
        }
        if !applied { return; }
        self.file_view.set_viewport(self.view_rect(), self.dpi);
        let names = self.file_view.file_names();
        self.mirror_selection(&names);
        self.refresh_status();
        self.invalidate_rect(self.layout.browser);
        self.invalidate_rect(self.layout.status);
    }

    /// Rectangle of the navigation tree in client pixels.
    pub(super) fn tree_rect(&self) -> RectF {
        let area = self.layout.tree;
        RectF::new(area.x as f32, area.y as f32, area.width as f32, area.height as f32)
    }

    /// True when ZSDesk paints the navigation tree.
    pub(super) fn custom_tree(&self) -> bool {
        self.session.ui.custom_view && self.session.ui.show_tree && !self.layout.tree.is_empty()
    }

    /// Tree hit test in client coordinates.
    pub(super) fn tree_hit(&self, p: POINT) -> nav_tree::NavHit {
        if !self.custom_tree() { return nav_tree::NavHit::Empty; }
        self.nav_tree.hit(p.x as f32, p.y as f32, self.tree_rect())
    }

    /// Click on a tree row: expanders toggle, labels navigate.
    pub(super) fn tree_click(&mut self, p: POINT) {
        if !self.custom_tree() { return; }
        match self.tree_hit(p) {
            nav_tree::NavHit::Chevron(index) => {
                self.nav_tree.toggle(index);
                self.invalidate_rect(self.layout.tree);
            }
            nav_tree::NavHit::Row(index) => {
                self.tree_focus = true;
                self.nav_tree.selection = Some(index);
                let Some(location) = self.nav_tree.selected_location() else { return };
                let _ = unsafe { SetFocus(Some(self.hwnd)) };
                unsafe { self.navigate_from_tree(location) };
            }
            nav_tree::NavHit::Empty => {}
        }
    }

    pub(super) fn tree_hover(&mut self, p: POINT) {
        if !self.custom_tree() { return; }
        let index = match self.tree_hit(p) { nav_tree::NavHit::Row(index) | nav_tree::NavHit::Chevron(index) => Some(index), nav_tree::NavHit::Empty => None };
        if self.nav_tree.hover() == index { return; }
        self.nav_tree.set_hover(index);
        self.invalidate_rect(self.layout.tree);
    }

    /// Scrolls the tree by `delta` pixels (positive moves the content up).
    pub(super) fn tree_scroll(&mut self, delta: f32) {
        if !self.custom_tree() { return; }
        self.nav_tree.scroll_by(delta);
        self.invalidate_rect(self.layout.tree);
    }

    /// Keyboard navigation inside the tree.
    pub(super) fn tree_key(&mut self, key: u16, shift: bool) -> bool {
        if !self.custom_tree() { return false; }
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_RETURN};
        match key {
            k if k == VK_DOWN.0 => { self.follow_tree_selection(1); true }
            k if k == VK_UP.0 => { self.follow_tree_selection(-1); true }
            k if k == VK_HOME.0 => { self.follow_tree_selection(isize::MIN / 2); true }
            k if k == VK_END.0 => { self.follow_tree_selection(isize::MAX / 2); true }
            k if k == VK_NEXT.0 => { let page = (self.tree_rect().h / self.nav_tree.row_height()).max(1.0) as isize; self.follow_tree_selection(page); true }
            k if k == VK_PRIOR.0 => { let page = (self.tree_rect().h / self.nav_tree.row_height()).max(1.0) as isize; self.follow_tree_selection(-page); true }
            k if k == VK_RIGHT.0 => {
                if let Some(index) = self.nav_tree.selection {
                    if !self.nav_tree.rows.get(index).is_some_and(|row| row.expanded) { self.nav_tree.expand(index); }
                    else { self.follow_tree_selection(1); }
                }
                self.invalidate_rect(self.layout.tree);
                true
            }
            k if k == VK_LEFT.0 && !shift => {
                if let Some(index) = self.nav_tree.selection {
                    if self.nav_tree.rows.get(index).is_some_and(|row| row.expanded) { self.nav_tree.collapse(index); }
                }
                self.invalidate_rect(self.layout.tree);
                true
            }
            k if k == VK_RETURN.0 => {
                if let Some(location) = self.nav_tree.selected_location() { unsafe { self.navigate_from_tree(location) }; }
                true
            }
            _ => false,
        }
    }

    /// Moves the tree selection and navigates to the row it landed on.
    fn follow_tree_selection(&mut self, delta: isize) {
        if let Some(location) = self.nav_tree.step(delta) { unsafe { self.navigate_from_tree(location) }; }
    }

    /// Re-enumerates the folder after a refresh, a drop or a file-system event.
    /// The rows already on screen stay until the new listing replaces them.
    pub(super) fn reload_file_view(&mut self) {
        if !self.custom_view { return; }
        self.file_view.set_viewport(self.view_rect(), self.dpi);
        self.start_file_list();
        self.invalidate_rect(self.layout.browser);
    }

    /// Copies the list selection into the hidden Shell view.
    pub(super) fn mirror_selection(&self, names: &[String]) {
        let Some(browser) = &self.browser else { return };
        if names.is_empty() { let _ = browser.clear_selection(); return; }
        let names: Vec<std::ffi::OsString> = names.iter().map(std::ffi::OsString::from).collect();
        let _ = unsafe { browser.select_children(&names) };
    }

    /// Pulls the Shell selection back; used after the Shell changed it itself
    /// (Ctrl+A, a verb, a delete).
    pub(super) fn adopt_shell_selection(&mut self) {
        if !self.custom_view { return; }
        let Some(browser) = &self.browser else { return };
        let names = browser.selected_names();
        self.file_view.adopt(&names);
        self.file_view.reveal_focus(self.dpi);
        self.invalidate_rect(self.layout.browser);
    }

    /// Hit test in client coordinates, when the pointer is over the file pane.
    pub(super) fn view_hit(&self, p: POINT) -> Option<file_view::ViewHit> {
        if !self.custom_view { return None; }
        let rect = self.view_rect();
        if !rect.contains(p.x as f32, p.y as f32) { return None; }
        Some(self.file_view.hit(rect, self.dpi, p.x, p.y))
    }

    /// Click on a row, the header or the empty area.
    pub(super) fn view_click(&mut self, p: POINT, control: bool, shift: bool) -> bool {
        self.tree_focus = false;
        let Some(hit) = self.view_hit(p) else { return false };
        match hit {
            file_view::ViewHit::Header(column) => {
                self.file_view.set_sort(column.key());
                self.mirror_selection(&[]);
            }
            file_view::ViewHit::Row(index) => {
                // A second click on the same row inside the double-click time
                // opens it, whether or not Windows turned it into a real
                // double-click for this window.
                if let Some((previous, at)) = self.last_click {
                    if previous == index && at.elapsed() <= double_click_span() {
                        self.last_click = None;
                        unsafe { self.open_view_row(index) };
                        return true;
                    }
                }
                self.last_click = Some((index, std::time::Instant::now()));
                let names = self.file_view.click(Some(index), control, shift);
                self.mirror_selection(&names);
            }
            file_view::ViewHit::Empty => {
                let _ = self.file_view.click(None, control, shift);
                self.mirror_selection(&[]);
            }
            file_view::ViewHit::ScrollBar { thumb } => {
                // A bar press is not a click on a row, so the double-click
                // tracker must not pair it with the previous row click.
                self.last_click = None;
                if self.file_view.press_bar(self.view_rect(), self.dpi, p.y as f32, thumb) {
                    unsafe { let _ = SetCapture(self.hwnd); }
                }
            }
            file_view::ViewHit::None => {}
        }
        self.refresh_status();
        self.invalidate_rect(self.layout.browser);
        true
    }

    /// Pointer moved over the pane: hover highlight only.
    pub(super) fn view_hover(&mut self, p: POINT) -> bool {
        let Some(hit) = self.view_hit(p) else {
            let mut redraw = self.file_view.set_hover(None);
            redraw |= self.file_view.set_bar_hot(false);
            if redraw { self.invalidate_rect(self.layout.browser); }
            return false;
        };
        let bar = matches!(hit, file_view::ViewHit::ScrollBar { .. });
        let mut redraw = self.file_view.set_bar_hot(bar);
        let hover = match hit { file_view::ViewHit::Row(index) => Some(index), _ => None };
        redraw |= self.file_view.set_hover(hover);
        if redraw { self.invalidate_rect(self.layout.browser); }
        true
    }

    /// Pointer moved with the scrollbar thumb held down.
    pub(super) fn view_drag_bar(&mut self, p: POINT) -> bool {
        if !self.custom_view || !self.file_view.dragging_bar() { return false; }
        self.file_view.drag_bar(self.view_rect(), self.dpi, p.y as f32);
        self.invalidate_rect(self.layout.browser);
        true
    }

    /// Releases the thumb; returns whether a drag was in progress.
    pub(super) fn end_view_drag_bar(&mut self) -> bool {
        if !self.file_view.end_bar_drag() { return false; }
        self.invalidate_rect(self.layout.browser);
        true
    }

    pub(super) fn view_wheel(&mut self, p: POINT) -> bool {
        if self.view_hit(p).is_none() { return false; }
        true
    }

    pub(super) fn view_scroll(&mut self, lines: f32) {
        let row = 24.0 * self.dpi as f32 / 96.0;
        self.file_view.scroll_by(lines * row * 3.0, self.dpi);
        self.invalidate_rect(self.layout.browser);
    }

    /// Opens a row: folders navigate, files go through their Shell association.
    pub(super) unsafe fn open_view_row(&mut self, index: usize) {
        let Some(entry) = self.file_view.items.get(index).cloned() else { return };
        if entry.is_dir {
            if let Some(path) = entry.path {
                unsafe { self.navigate_to(ShellLocation::Path(path)) };
                return;
            }
            if let Some(item) = folder_items::item_of(&entry) {
                if let Some(location) = unsafe { zsdesk_windows::shell::location_from_shell_item(&item) } {
                    unsafe { self.navigate_to(location) };
                }
            }
            return;
        }
        if let Some(path) = entry.path {
            unsafe { zsdesk_windows::shell::open_path(self.hwnd, &path) };
            return;
        }
        // Items without a filesystem path (Recycle Bin, virtual folders) can
        // still be opened: ask the Shell for their default verb.
        if let Some(pidl) = folder_items::pidl_of(&entry) {
            if let Ok(menu) = zsdesk_windows::shell::menu::ShellMenu::for_items(self.hwnd, &[pidl]) {
                let _ = menu.invoke_verb(self.hwnd, "open", false);
            }
        }
    }

    /// Double click: open the clicked row.
    pub(super) unsafe fn view_double_click(&mut self, p: POINT) -> bool {
        match self.view_hit(p) {
            Some(file_view::ViewHit::Row(index)) => {
                self.last_click = None;
                unsafe { self.open_view_row(index) };
                true
            }
            _ => false,
        }
    }

    /// Context menu over the pane. The Shell builds it from the hidden view's
    /// selection, which was just updated to the clicked row.
    pub(super) unsafe fn view_context_menu(&mut self, p: POINT) -> bool {
        let Some(hit) = self.view_hit(p) else { return false };
        if let file_view::ViewHit::Row(index) = hit {
            if !self.file_view.selection.contains(&index) {
                let names = self.file_view.click(Some(index), false, false);
                self.mirror_selection(&names);
            }
        }
        let Some(browser) = &self.browser else { return false };
        let Ok(view) = (unsafe { browser.current_view() }) else { return false };
        let mut screen = p;
        let _ = unsafe { ClientToScreen(self.hwnd, &mut screen) };
        // context_menu::show reads a packed client point as a signed pair.
        let packed = ((screen.y as isize & 0xffff) << 16) | (screen.x as isize & 0xffff);
        let _ = unsafe { context_menu::show(self.hwnd, &view, LPARAM(packed)) };
        true
    }

    /// Keyboard navigation. Everything else (copy, paste, delete, rename…) still
    /// goes to the Shell view, which acts on the mirrored selection.
    pub(super) fn view_key(&mut self, key: u16, shift: bool, control: bool) -> bool {
        if !self.custom_view || unsafe { GetFocus() } != self.hwnd { return false; }
        let row = 24.0 * self.dpi as f32 / 96.0;
        let page = ((self.view_rect().h / row) as isize - 1).max(1);
        match key {
            k if k == VK_DOWN.0 => self.file_view.step(1, shift),
            k if k == VK_UP.0 => self.file_view.step(-1, shift),
            k if k == VK_NEXT.0 => self.file_view.step(page, shift),
            k if k == VK_PRIOR.0 => self.file_view.step(-page, shift),
            k if k == VK_HOME.0 => self.file_view.step(isize::MIN / 2, shift),
            k if k == VK_END.0 => self.file_view.step(isize::MAX / 2, shift),
            k if k == VK_A.0 && control => self.file_view.select_all(),
            k if k == VK_RETURN.0 && !control => {
                if let Some(index) = self.file_view.focus() { unsafe { self.open_view_row(index) }; }
                return true;
            }
            _ => return false,
        }
        let names = self.file_view.file_names();
        self.mirror_selection(&names);
        self.file_view.reveal_focus(self.dpi);
        self.refresh_status();
        self.invalidate_rect(self.layout.browser);
        true
    }

    /// Icons for the visible rows, uploaded before the drawing closure runs.
    pub(super) fn prepare_view_icons(&mut self) -> Vec<Option<ID2D1Bitmap>> {
        if !self.custom_view { return Vec::new(); }
        let range = self.file_view.visible_rows(self.view_rect(), self.dpi);
        let icons: Vec<i32> = range.filter_map(|index| self.file_view.items.get(index).map(|entry| entry.icon)).collect();
        icons.into_iter().map(|index| self.item_icon(index)).collect()
    }

    /// Icons for the visible tree rows, plus the index of the first one.
    pub(super) fn prepare_tree_icons(&mut self) -> (usize, Vec<Option<ID2D1Bitmap>>) {
        if !self.custom_tree() { return (0, Vec::new()); }
        let rect = self.tree_rect();
        self.nav_tree.set_viewport(rect.w.max(0.0) as i32, rect.h.max(0.0) as i32, self.dpi);
        let (first, last) = self.nav_tree.visible_rows();
        let icons: Vec<i32> = self.nav_tree.rows[first.min(self.nav_tree.rows.len())..last.min(self.nav_tree.rows.len())]
            .iter().map(|row| row.icon).collect();
        (first, icons.into_iter().map(|index| self.item_icon(index)).collect())
    }

    /// Cached system-image-list icon, keyed by index and pixel size.
    pub(super) fn item_icon(&mut self, index: i32) -> Option<ID2D1Bitmap> {
        let px = self.s(paint::ICON_DIP);
        let key = format!("sys|{px}|{index}");
        let gfx = self.gfx.as_ref()?;
        let generation = gfx.generation();
        if !self.icons.contains_key(&key) {
            let wic = zsdesk_windows::shell::icons::system_icon(index, px).and_then(|icon| {
                let wic = gfx.wic_from_icon(icon);
                let _ = unsafe { DestroyIcon(icon) };
                wic
            });
            if self.icons.len() > 512 { self.icons.clear(); }
            self.icons.insert(key.clone(), paint::IconEntry { wic, bitmap: None });
        }
        let entry = self.icons.get_mut(&key)?;
        if entry.bitmap.as_ref().is_none_or(|(cached, _)| *cached != generation) {
            entry.bitmap = entry.wic.as_ref().and_then(|source| gfx.upload(source)).map(|bitmap| (generation, bitmap));
        }
        entry.bitmap.as_ref().map(|(_, bitmap)| bitmap.clone())
    }
}

impl WindowState {
    /// Opens the in-place rename editor over a painted row.
    pub(super) unsafe fn begin_view_rename(&mut self, index: usize) {
        let Some(entry) = self.file_view.items.get(index).cloned() else { return };
        // Virtual items have no path of their own; their Shell view renames them.
        if entry.path.is_none() { return; }
        if self.view_rename.0.is_null() {
            let Ok(edit) = (unsafe { child(self.hwnd, "EDIT", "", ID_VIEW_RENAME, ES_AUTOHSCROLL as u32, true) }) else { return };
            self.view_rename = edit;
            unsafe {
                SendMessageW(edit, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
                let _ = SetWindowSubclass(edit, Some(edit_proc), ID_VIEW_RENAME, self.hwnd.0 as usize);
                theme::theme_control(edit, self.dark);
                let (normal, focused, text) = (colorref(self.palette.input), colorref(self.palette.input_focused), colorref(self.palette.text));
                let _ = replace_edit_colors(edit, normal, focused, text);
            }
        }
        self.view_rename_index = Some(index);
        let rect = self.view_rect();
        // The editor must line up with the painted cell, so it uses the same
        // pane width the rows are drawn in (the scrollbar lane aside).
        let content = self.file_view.content_rect(rect, self.dpi);
        let row = self.file_view.row_rect(content, self.dpi, index);
        let name_column = self.file_view.columns(RectF::new(content.x, content.y, content.w, row.h), self.dpi)[0].1;
        let line = self.edit_line_height();
        let area = LayoutRect::new(name_column.x as i32 + self.s(26),
            row.y as i32 + (row.h as i32 - line) / 2,
            (name_column.w as i32 - self.s(32)).max(self.s(80)), line);
        self.place_edit(self.view_rename, area);
        let name = entry.name.clone();
        let text = wide(&name);
        unsafe {
            let _ = SetWindowTextW(self.view_rename, PCWSTR(text.as_ptr()));
            let _ = ShowWindow(self.view_rename, SW_SHOW);
            let _ = SetFocus(Some(self.view_rename));
        }
        // Select the stem so typing replaces the name but keeps the extension.
        let stem = name.rfind('.').filter(|dot| *dot > 0).unwrap_or(name.len());
        unsafe { SendMessageW(self.view_rename, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(stem as isize))); }
    }

    /// Renames the item being edited. Returns false when no editor was open.
    pub(super) unsafe fn commit_view_rename(&mut self) -> bool {
        let Some(index) = self.view_rename_index.take() else { return false };
        let text = if self.view_rename.0.is_null() { String::new() } else { super::navigation::window_text(self.view_rename) };
        if !self.view_rename.0.is_null() { let _ = unsafe { ShowWindow(self.view_rename, SW_HIDE) }; }
        let Some(entry) = self.file_view.items.get(index).cloned() else { return true };
        let Some(path) = entry.path else { return true };
        let name = text.trim().to_string();
        if !name.is_empty() && name != entry.name && !name.contains(['\\', '/']) {
            let target = match path.parent() {
                Some(parent) => parent.join(&name),
                None => PathBuf::from(&name),
            };
            if target.exists() {
                self.show_message(format!("已存在同名项目：{name}"));
            } else if let Err(error) = std::fs::rename(&path, &target) {
                self.show_message(format!("重命名失败：{error}"));
            }
        }
        self.reload_file_view();
        self.focus_view();
        true
    }

    pub(super) unsafe fn cancel_view_rename(&mut self) -> bool {
        if self.view_rename_index.take().is_none() { return false; }
        if !self.view_rename.0.is_null() { let _ = unsafe { ShowWindow(self.view_rename, SW_HIDE) }; }
        self.focus_view();
        true
    }
}

/// The system double-click interval, with a floor so a very short setting
/// still lets an ordinary double click open a row.
fn double_click_span() -> std::time::Duration {
    std::time::Duration::from_millis(unsafe { GetDoubleClickTime() }.max(500) as u64)
}
