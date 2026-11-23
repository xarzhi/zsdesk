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

    /// Location shown by the self-drawn list, when it is in charge.
    ///
    /// Search results are drawn here too. They used to be handed to the Shell
    /// view, which put a system-painted list in the middle of a self-drawn
    /// window — a different background, different text colours, unreadable in
    /// dark mode — and cost one Shell item per result before a single row could
    /// appear. The rows now come straight from the index.
    fn custom_view_location(&self) -> Option<ShellLocation> {
        if !self.session.ui.custom_view { return None; }
        Some(self.current_location().clone())
    }

    /// Loads the current folder and shows or hides the Shell view to match.
    pub(super) fn sync_file_view(&mut self) {
        // The search box names the folder it searches; a tab that moved has to
        // say so, whether or not the drawn list owns the pane.
        self.update_search_cue();
        let enabled = self.custom_view_location().is_some();
        // The status bar keeps its two view-mode buttons in both modes: the
        // drawn list honours them now, so switching modes never needs the menu.
        if self.custom_view != enabled {
            self.custom_view = enabled;
            self.relayout();
            self.position_children();
        }
        // The OLE drop target reads the folder from shared state: it is called
        // from inside the drag loop, where the window state cannot be borrowed.
        *self.drop_folder.borrow_mut() = if enabled {
            match self.current_location() {
                ShellLocation::Path(path) => Some(path.clone()),
                _ => None,
            }
        } else { None };
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
        // The table keeps the widths the user dragged, whatever folder the
        // window moves to next.
        self.file_view.set_column_widths(self.session.ui.details_columns);
        self.file_view.set_viewport(self.view_rect(), self.dpi);
        self.start_file_list();
        self.mirror_selection(&[]);
    }

    /// Starts a background listing of the folder on screen. The pane shows
    /// "正在读取…" until the result arrives, so a large folder or a slow share
    /// never freezes the window.
    fn start_file_list(&mut self) {
        // Images belong to the folder that produced them: a new listing starts
        // with an empty cache so rows never wear another folder's thumbnail.
        if !self.view_images.is_empty() || !self.view_images_wanted.is_empty() {
            self.view_images.clear();
            self.view_images_wanted.clear();
        }
        let location = self.current_location().clone();
        // Row indices belong to the listing that produced them, so an editor
        // left open over the old rows must not survive into a new listing.
        unsafe { self.cancel_view_rename(); }
        // Results come from the index, one page at a time: there is no folder
        // to read here, and asking the Shell to enumerate a search folder is
        // what made typing wait for the old results to be dropped first.
        if matches!(location, ShellLocation::Search { .. }) {
            self.ensure_search_rows(&location);
            return;
        }
        let token = self.file_view.begin_load(&location);
        self.search_token = token;
        self.search_paths.clear();
        self.search_seen.clear();
        self.search_queue.clear();
        self.search_listing = false;
        self.file_view.set_empty_message(None);
        // 查看 → 隐藏的项目 is a Shell-wide switch; the drawn list reads the
        // same flag so both views of a folder agree on what is in it.
        let (include_hidden, _) = zsdesk_windows::shell::commands::folder_settings();
        self.list_loader.start(token, location, include_hidden);
    }

    /// True when the drawn list owns the pane and the pane is a search result.
    pub(super) fn drawn_search(&self) -> bool {
        self.custom_view && matches!(self.current_location(), ShellLocation::Search { .. })
    }

    /// Starts a new page of results: the rows go away and every query behind
    /// them is dropped, so nothing from the previous page can land in the new
    /// one. A page keeps the same location (the query is unchanged), which is
    /// why this cannot be left to `begin_load`'s folder comparison.
    pub(super) fn reset_search_rows(&mut self) {
        let location = self.current_location().clone();
        self.search_token = self.file_view.restart(&location);
        self.search_paths.clear();
        self.search_seen.clear();
        self.search_queue.clear();
        self.search_listing = false;
        self.fast_found = 0;
        self.file_view.set_viewport(self.view_rect(), self.dpi);
        self.file_view.set_empty_message(Some("正在搜索…".into()));
        self.refresh_status();
        self.invalidate_rect(self.layout.browser);
        self.invalidate_rect(self.layout.status);
    }

    /// Makes sure the table belongs to `location` without throwing away rows
    /// that already arrived: a re-sync inside one query used to blank the pane.
    fn ensure_search_rows(&mut self, location: &ShellLocation) {
        if self.file_view.location.as_ref() == Some(location) {
            self.file_view.set_viewport(self.view_rect(), self.dpi);
            return;
        }
        self.reset_search_rows();
    }

    /// Hands the next batch of result paths to the row builder. One batch is
    /// in flight at a time: the worker reads metadata, and queueing every page
    /// at once would only make the queue longer.
    pub(super) fn start_queued_search_rows(&mut self) {
        if self.search_listing || self.search_queue.is_empty() { return; }
        let (include_hidden, _) = zsdesk_windows::shell::commands::folder_settings();
        let batch = std::mem::take(&mut self.search_queue);
        self.search_listing = true;
        self.list_loader.start_paths(self.search_token, batch, include_hidden);
    }

    /// Queues one page of index hits as rows of the drawn list.
    pub(super) fn queue_search_rows(&mut self, paths: &[PathBuf]) {
        for path in paths {
            let key = ExplorerBrowserHost::search_path_key(path);
            if !self.search_seen.insert(key) { continue; }
            self.search_paths.push(path.clone());
            self.search_queue.push(path.clone());
        }
        self.start_queued_search_rows();
    }

    /// Applies every listing that finished on a worker thread.
    pub(super) fn apply_listed_folders(&mut self) {
        let search = matches!(self.current_location(), ShellLocation::Search { .. });
        let mut applied = false;
        for listing in self.list_loader.drain() {
            if search {
                // Result pages accumulate instead of replacing each other: the
                // next page of the same query must not throw away this one.
                self.search_listing = false;
                applied |= self.file_view.append_load(listing.token, listing.items);
            } else {
                applied |= self.file_view.finish_load(listing.token, listing.items);
            }
        }
        if search {
            self.start_queued_search_rows();
            // The status bar counts what the table shows, not what the index
            // offered: rows the user can see are the ones that were answered.
            self.fast_found = self.file_view.items.len();
            // The text was built when the batch was queued, before the rows
            // existed; without this the line keeps saying "当前页 0 项".
            if let Some(event) = self.fast_last.clone() {
                self.search_progress = Some(format!("{}{}", super::global_search::progress_text(&event, self.fast_found),
                    self.search_scope_note()));
            }
            if !self.search_controls().busy {
                self.file_view.set_empty_message(Some("没有匹配的项目".into()));
            }
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

    /// Re-reads the pinned quick-access folders from the Shell.
    ///
    /// The pane is built once from the Shell namespace, so a pin or an unpin
    /// made anywhere — this pane's own menu, a folder's menu, Explorer — would
    /// otherwise keep the old rows until the window was reopened.
    pub(super) fn refresh_nav_tree(&mut self) {
        if !self.custom_tree() { return; }
        if self.nav_tree.reload() { self.invalidate_rect(self.layout.tree); }
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

    /// Native context menu for the row under the pointer.
    ///
    /// Explorer's pane menu is the Shell's own: 固定到快速访问, 管理, 格式化为,
    /// 清空回收站 and the extension verbs are all decided by the item, so the row
    /// hands its PIDL over instead of reproducing any of them here. A right
    /// click selects the row without navigating, exactly like the system pane.
    pub(super) unsafe fn tree_context_menu(&mut self, p: POINT, index: usize) {
        if !self.custom_tree() { return; }
        let Some(location) = self.nav_tree.location_of(index) else { return };
        if self.nav_tree.selection != Some(index) {
            self.nav_tree.selection = Some(index);
            self.invalidate_rect(self.layout.tree);
        }
        self.tree_focus = true;
        // A pinned folder is an item of the quick-access namespace: handing the
        // Shell that item is what makes the menu say 从快速访问取消固定 instead
        // of 固定到快速访问. Everything else has only its path.
        let pidl = self.nav_tree.pidl_of(index)
            .or_else(|| zsdesk_windows::shell::pidl::location_pidl(&location));
        let Some(pidl) = pidl else { return };
        let mut screen = p;
        let _ = unsafe { ClientToScreen(self.hwnd, &mut screen) };
        // context_menu::point reads the packed pair back as signed 16-bit.
        let packed = ((screen.y as isize & 0xffff) << 16) | (screen.x as isize & 0xffff);
        if let Err(error) = context_menu::show_item(self.hwnd, pidl, Some(location), LPARAM(packed)) {
            self.show_message(format!("无法打开系统菜单：{error}"));
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
        // A rename editor in the pane rides on its row, not on the pane.
        self.sync_rename_editor();
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
        // Results are a query, not a folder: re-listing would mean running the
        // query again and losing the selection. Dropping the rows whose files
        // are gone is enough for the delete and rename that end up here.
        if matches!(self.current_location(), ShellLocation::Search { .. }) {
            if self.file_view.retain_existing() { self.refresh_status(); }
            self.invalidate_rect(self.layout.browser);
            return;
        }
        self.start_file_list();
        self.invalidate_rect(self.layout.browser);
    }

    /// Copies the list selection into the hidden Shell view.
    ///
    /// Search rows have no counterpart there: the results folder is empty until
    /// a menu or a verb asks for it, and selecting by name would only fail on a
    /// folder that is not a directory.
    pub(super) fn mirror_selection(&self, names: &[String]) {
        if matches!(self.current_location(), ShellLocation::Search { .. }) { return; }
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
            file_view::ViewHit::HeaderDivider(_) => {
                // The boundary owns the press: sizing a column must never be
                // mistaken for a sort, whatever the double-click tracker holds.
                self.last_click = None;
                if self.file_view.press_divider(self.view_rect(), self.dpi, p.x as f32) {
                    unsafe { let _ = SetCapture(self.hwnd); }
                }
            }
            file_view::ViewHit::Header(column) => {
                self.file_view.set_sort(column.key());
                // Sorting re-orders the rows; the selection follows its files.
                self.mirror_selection(&self.file_view.file_names());
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
                // The press may still become a drag: remember where it started
                // and keep the moves coming even when the pointer leaves.
                self.view_press = Some((p, index));
                unsafe { let _ = SetCapture(self.hwnd); }
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
            redraw |= self.file_view.set_divider_hot(None);
            if redraw { self.invalidate_rect(self.layout.browser); }
            return false;
        };
        let bar = matches!(hit, file_view::ViewHit::ScrollBar { .. });
        let mut redraw = self.file_view.set_bar_hot(bar);
        let hover = match hit { file_view::ViewHit::Row(index) => Some(index), _ => None };
        redraw |= self.file_view.set_hover(hover);
        // A column boundary lights up under the pointer: the header has no
        // printed rule, so this is what tells the user the column can be sized.
        let divider = match hit { file_view::ViewHit::HeaderDivider(index) => Some(index), _ => None };
        redraw |= self.file_view.set_divider_hot(divider);
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

    /// Pointer moved with a column boundary held down. The header follows the
    /// pointer even outside the pane, so a column can be sized without
    /// fighting the edge of the window.
    pub(super) fn view_drag_divider(&mut self, p: POINT) -> bool {
        if !self.custom_view || !self.file_view.dragging_divider() { return false; }
        self.file_view.drag_divider(self.view_rect(), self.dpi, p.x as f32);
        self.invalidate_rect(self.layout.browser);
        true
    }

    /// Releases the boundary and stores the widths it left behind, so the next
    /// window — and the next launch — shows the same table.
    pub(super) fn end_view_drag_divider(&mut self) -> bool {
        if !self.file_view.end_divider_drag() { return false; }
        self.session.ui.details_columns = self.file_view.column_widths_dip();
        self.save();
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
        let step = self.file_view.scroll_step(self.dpi);
        self.file_view.scroll_by(lines * step * 3.0, self.dpi);
        // The rows moved under the editor: put it back on its name.
        self.sync_rename_editor();
        self.invalidate_rect(self.layout.browser);
    }

    /// Shell item id lists of the selected rows, for an OLE drag. Items without
    /// a filesystem path (Recycle Bin, virtual folders) still resolve through
    /// their parsing name, so they can be dragged too.
    pub(super) fn drag_pidls(&self) -> Vec<Vec<u8>> {
        self.file_view.selection.iter()
            .filter_map(|index| self.file_view.items.get(*index))
            .filter_map(folder_items::pidl_of)
            .take(2000)
            .collect()
    }

    /// Folder a drop at `p` lands in: a folder row, a tree row, or the folder
    /// on screen. `None` for locations that have no filesystem folder.
    pub(super) fn drop_folder(&self, p: POINT) -> Option<PathBuf> {
        if let Some(file_view::ViewHit::Row(index)) = self.view_hit(p) {
            if let Some(entry) = self.file_view.items.get(index) {
                if entry.is_dir {
                    if let Some(path) = &entry.path { return Some(path.clone()); }
                }
            }
        }
        if self.custom_tree() && self.tree_rect().contains(p.x as f32, p.y as f32) {
            if let nav_tree::NavHit::Row(index) = self.tree_hit(p) {
                if let Some(ShellLocation::Path(path)) = self.nav_tree.location_of(index) { return Some(path); }
            }
        }
        match self.current_location() {
            ShellLocation::Path(path) => Some(path.clone()),
            _ => None,
        }
    }

    /// Turns a held press into a Shell drag once the pointer has clearly left
    /// the row it started on.
    ///
    /// The rows are self-drawn, so no Shell view ever sees the press and cannot
    /// start a drag of its own. The data object is built from the rows' parsing
    /// names, which covers virtual items too, and `DoDragDrop` then behaves
    /// exactly as it does for a drag out of File Explorer: move inside one
    /// volume, copy across volumes, links with Alt, cancels with Escape.
    ///
    /// Returns `true` when a drag was started, in which case the click that
    /// armed it must not also open or rename anything.
    pub(super) fn drag_view(&mut self, p: POINT) -> bool {
        let Some((start, index)) = self.view_press else { return false };
        let width = unsafe { GetSystemMetricsForDpi(SM_CXDRAG, self.dpi) }.max(2);
        let height = unsafe { GetSystemMetricsForDpi(SM_CYDRAG, self.dpi) }.max(2);
        if (p.x - start.x).abs() < width && (p.y - start.y).abs() < height { return false; }
        self.view_press = None;
        let _ = unsafe { ReleaseCapture() };
        // Dragging a row that is not part of the selection drags just that row;
        // dragging inside it drags the whole selection, as Explorer does.
        let indices = drag_rows(&self.file_view.selection, index);
        let mut pidls = Vec::new();
        for name in self.file_view.parsing_names(&indices) {
            let Ok(item) = zsdesk_windows::shell::pidl::item_from_parsing_name(&name) else { continue };
            if let Some(pidl) = zsdesk_windows::shell::pidl::item_pidl(&item) { pidls.push(pidl); }
        }
        if pidls.is_empty() { return false; }
        // A drag is not a click: the row must not open on the release that
        // ends it, nor pair with the next click as a double-click.
        self.last_click = None;
        let payload = Box::into_raw(Box::new(pidls));
        if unsafe { PostMessageW(Some(self.hwnd), WM_BEGIN_DRAG, WPARAM(0), LPARAM(payload as isize)) }.is_err() {
            unsafe { drop(Box::from_raw(payload)); }
            return false;
        }
        true
    }

    /// Folder row under the pointer, outlined while a drag hovers over it.
    pub(super) fn drop_row(&self, p: POINT) -> Option<usize> {
        let Some(file_view::ViewHit::Row(index)) = self.view_hit(p) else { return None };
        self.file_view.items.get(index).is_some_and(|entry| entry.is_dir && entry.path.is_some()).then_some(index)
    }

    /// Files dropped on the window: copy or move them into the folder under the
    /// pointer, then let the Shell's own operation do the work.
    pub(super) fn drop_files(&mut self, dropped: super::DroppedFiles) {
        let point = POINT { x: dropped.x, y: dropped.y };
        let destination = self.drop_folder(point);
        let Some(destination) = destination else {
            self.show_message("这个位置不能接收文件".into());
            return;
        };
        // Dropping a selection back where it came from: nothing to do, and the
        // Shell would only ask about overwriting itself.
        if dropped.moving && all_inside(&dropped.paths, &destination) { return; }
        if let Err(error) = zsdesk_windows::shell::file_transfer::transfer_paths(self.hwnd, dropped.paths, destination, dropped.moving) {
            self.show_message(format!("无法开始文件操作：{error}"));
        }
    }

    /// Outline the folder row a drag is hovering over, or clear it.
    pub(super) fn drop_highlight(&mut self, p: Option<POINT>) {
        let row = p.and_then(|point| self.drop_row(point));
        let mut redraw = self.file_view.set_drop_target(row);
        if self.custom_tree() {
            let tree_row = p.and_then(|point| match self.tree_hit(point) { nav_tree::NavHit::Row(index) => Some(index), _ => None });
            redraw |= self.nav_tree.set_drop_target(tree_row);
        }
        if redraw {
            self.invalidate_rect(self.layout.browser);
            if self.custom_tree() { self.invalidate_rect(self.layout.tree); }
        }
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

    /// Selected rows as the Shell wants them: their identity, plus the folder
    /// "打开" should land in for a row that stands for one.
    fn selection_items(&self) -> Vec<(Vec<u8>, Option<ShellLocation>)> {
        let mut items = Vec::new();
        for index in &self.file_view.selection {
            let Some(entry) = self.file_view.items.get(*index) else { continue };
            let Some(pidl) = folder_items::pidl_of(entry) else { continue };
            let target = if entry.is_dir { entry.path.clone().map(ShellLocation::Path) } else { None };
            items.push((pidl, target));
            if items.len() >= 128 { break; }
        }
        items
    }

    /// Context menu over the pane. The Shell builds it from the hidden view's
    /// selection, which was just updated to the clicked row.
    ///
    /// Search rows are the exception: their folder is a streamed result set
    /// that only exists while the Shell is filling it, so the rows hand their
    /// own identities over instead — the same way the navigation pane does.
    pub(super) unsafe fn view_context_menu(&mut self, p: POINT) -> bool {
        let Some(hit) = self.view_hit(p) else { return false };
        if let file_view::ViewHit::Row(index) = hit {
            if !self.file_view.selection.contains(&index) {
                let names = self.file_view.click(Some(index), false, false);
                self.mirror_selection(&names);
            }
        }
        let mut screen = p;
        let _ = unsafe { ClientToScreen(self.hwnd, &mut screen) };
        // context_menu::show reads a packed client point as a signed pair.
        let packed = ((screen.y as isize & 0xffff) << 16) | (screen.x as isize & 0xffff);
        if matches!(self.current_location(), ShellLocation::Search { .. }) {
            let items = self.selection_items();
            if items.is_empty() { return false; }
            if let Err(error) = context_menu::show_items(self.hwnd, items, LPARAM(packed)) {
                self.show_message(format!("无法打开系统菜单：{error}"));
            }
            return true;
        }
        let Some(browser) = &self.browser else { return false };
        let Ok(view) = (unsafe { browser.current_view() }) else { return false };
        let _ = unsafe { context_menu::show(self.hwnd, &view, LPARAM(packed)) };
        true
    }

    /// Keyboard file commands for the drawn rows.
    ///
    /// The hidden Shell view holds no items while a search runs, so the usual
    /// route through its selection would quietly do nothing. The rows carry
    /// their own identity instead, exactly like the context menu does.
    /// `false` means "not this pane", so the caller falls back to the Shell.
    pub(super) fn search_shortcut(&mut self, action: super::keyboard::FileShortcut) -> bool {
        use super::keyboard::FileShortcut;
        use zsdesk_windows::shell::file_transfer;
        // F2 in a drawn folder renames the row in place, with the editor this
        // pane owns. Sent to the hidden Shell view it would rename inside a
        // window nobody can see, which read as "F2 does nothing".
        if action == FileShortcut::Rename && self.custom_view {
            if let Some(index) = self.file_view.focus() { unsafe { self.begin_view_rename(index) }; }
            return true;
        }
        if !self.drawn_search() { return false; }
        if action == FileShortcut::Paste {
            // Explorer pastes into the folder the search was started from.
            let origin = self.search_controls().origin.clone().and_then(|it| it.as_path().map(std::path::PathBuf::from));
            match origin {
                Some(folder) => self.shell_result(Some(file_transfer::paste(self.hwnd, &folder))),
                None => self.show_message("没有可以粘贴到的文件夹".into()),
            }
            return true;
        }
        let items = self.selection_items();
        if items.is_empty() { return true; }
        let pidls: Vec<Vec<u8>> = items.into_iter().map(|(pidl, _)| pidl).collect();
        let result = match action {
            FileShortcut::Copy => Some(file_transfer::copy_items(self.hwnd, &pidls, false)),
            FileShortcut::Cut => Some(file_transfer::copy_items(self.hwnd, &pidls, true)),
            FileShortcut::Delete { permanent } => match file_transfer::paths_from_pidls(&pidls) {
                Ok(paths) => Some(file_transfer::delete_paths(self.hwnd, paths, permanent)),
                Err(error) => { self.show_message(format!("无法删除：{error}")); None }
            },
            // 属性 opens a modal sheet with its own message loop; it keeps its
            // route through the row menu, which runs outside the state borrow.
            _ => return false,
        };
        self.shell_result(result);
        true
    }

    /// Keyboard navigation. Everything else (copy, paste, delete, rename…) still
    /// goes to the Shell view, which acts on the mirrored selection.
    pub(super) fn view_key(&mut self, key: u16, shift: bool, control: bool) -> bool {
        if !self.custom_view || unsafe { GetFocus() } != self.hwnd { return false; }
        let page = self.file_view.page_rows(self.dpi);
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
    ///
    /// Pictures and videos show a real thumbnail and every other file shows the
    /// icon it carries itself (a program's own icon, not a per-extension
    /// placeholder). Both come from the background worker, so until it answers
    /// the row keeps the system-image-list icon and never looks empty.
    pub(super) fn prepare_view_icons(&mut self) -> Vec<Option<ID2D1Bitmap>> {
        if !self.custom_view { return Vec::new(); }
        let range = self.file_view.visible_rows(self.view_rect(), self.dpi);
        // A 96 DIP icon mode asks for a 96 px thumbnail; asking for 16 px and
        // scaling it up would show a blurry square where a picture should be.
        let px = self.file_view.layout().icon_px(self.dpi);
        // Snapshot the rows first: the image cache is written while painting.
        let rows: Vec<(i32, Option<PathBuf>, String, bool)> = range
            .filter_map(|index| self.file_view.items.get(index)
                .map(|entry| (entry.icon, entry.path.clone(), entry.name.clone(), entry.is_dir)))
            .collect();
        rows.into_iter().map(|(icon, path, name, is_dir)| {
            path.filter(|_| !is_dir)
                .and_then(|path| self.view_image(&path, &name, px))
                .or_else(|| self.item_icon_size(icon, px as i32))
        }).collect()
    }

    /// Cached thumbnail or per-file icon, queuing the Shell work on first sight.
    fn view_image(&mut self, path: &std::path::Path, name: &str, px: u32) -> Option<ID2D1Bitmap> {
        let thumbnail = zsdesk_windows::shell::icons::wants_thumbnail(name);
        let key = format!("{}|{px}|{}", if thumbnail { 't' } else { 'i' }, path.display());
        if !self.view_images.contains_key(&key) {
            // The key is remembered even while the Shell has it queued: a
            // repaint must not post the same request again.
            if self.view_images_wanted.insert(key.clone()) {
                let request = zsdesk_windows::shell::icons::IconRequest::for_path(
                    key.clone(), path.to_path_buf(), px.max(1), thumbnail, image_token(&key));
                if !self.images.as_ref().is_some_and(|loader| loader.request(request)) {
                    self.view_images_wanted.remove(&key);
                }
            }
            return None;
        }
        let gfx = self.gfx.as_ref()?;
        let generation = gfx.generation();
        let entry = self.view_images.get_mut(&key)?;
        if entry.bitmap.as_ref().is_none_or(|(cached, _)| *cached != generation) {
            entry.bitmap = entry.wic.as_ref().and_then(|source| gfx.upload(source)).map(|bitmap| (generation, bitmap));
        }
        entry.bitmap.as_ref().map(|(_, bitmap)| bitmap.clone())
    }

    /// Moves finished thumbnails from the worker into the image cache.
    pub(super) fn apply_view_images(&mut self, token: usize) {
        let ready = {
            let Some(loader) = self.images.as_ref() else { return };
            if !loader.accepts_notification(token) { return; }
            loader.take_ready()
        };
        if ready.is_empty() { return; }
        {
            let Some(gfx) = self.gfx.as_ref() else { return };
            for result in ready {
                let wic = (!result.bitmap.is_invalid())
                    .then(|| gfx.wic_from_hbitmap(result.bitmap, result.alpha))
                    .flatten();
                self.view_images_wanted.remove(&result.key);
                // The cache is bounded; dropping it also forgets which keys were
                // asked for, or the evicted rows could never load again.
                if self.view_images.len() > 512 {
                    self.view_images.clear();
                    self.view_images_wanted.clear();
                }
                self.view_images.insert(result.key.clone(), paint::IconEntry { wic, bitmap: None });
            }
        }
        self.invalidate_rect(self.layout.browser);
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
        self.item_icon_size(index, self.s(paint::ICON_DIP))
    }

    /// The same, at the pixel size a view mode asks for.
    fn item_icon_size(&mut self, index: i32, px: i32) -> Option<ID2D1Bitmap> {
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
    /// Creates the in-place rename editor on first use.
    ///
    /// The list and the navigation pane share it: both rename a name in place,
    /// and both want the same font, theme and keyboard handling.
    unsafe fn ensure_view_rename_edit(&mut self) -> bool {
        if !self.view_rename.0.is_null() { return true; }
        let Ok(edit) = (unsafe { child(self.hwnd, "EDIT", "", ID_VIEW_RENAME, ES_AUTOHSCROLL as u32, true) }) else { return false };
        self.view_rename = edit;
        unsafe {
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
            let _ = SetWindowSubclass(edit, Some(edit_proc), ID_VIEW_RENAME, self.hwnd.0 as usize);
            theme::theme_control(edit, self.dark);
            let (normal, focused, text) = (colorref(self.palette.input), colorref(self.palette.input_focused), colorref(self.palette.text));
            let _ = replace_edit_colors(edit, normal, focused, text);
        }
        true
    }

    /// Opens the in-place rename editor over a painted row.
    pub(super) unsafe fn begin_view_rename(&mut self, index: usize) {
        let Some(entry) = self.file_view.items.get(index).cloned() else { return };
        // Virtual items have no path of their own; their Shell view renames them.
        if entry.path.is_none() { return; }
        let rect = self.view_rect();
        // The editor sits on the exact rectangle the name is painted into, so
        // the text does not shift when the panel opens. Everything else (icon
        // column, centred labels in the icon layouts) follows from that rect.
        let Some(name) = self.file_view.name_rect(rect, self.dpi, index) else { return };
        if !unsafe { self.ensure_view_rename_edit() } { return }
        self.tree_rename_index = None;
        self.view_rename_index = Some(index);
        self.file_view.set_editing(Some(index));
        self.nav_tree.set_editing(None);
        let line = self.edit_line_height();
        let area = rename_area(name, line);
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
        // The row's painted name goes away with this frame; without the repaint
        // the old letters would stay on screen under the editor.
        self.invalidate();
    }

    /// Keeps an open rename editor on the name it covers.
    ///
    /// The pane scrolls and resizes under the editor, so its box has to follow
    /// the row; otherwise the field floats over whatever moved into its place.
    pub(super) fn sync_rename_editor(&mut self) {
        if self.view_rename.0.is_null() || !unsafe { IsWindowVisible(self.view_rename) }.as_bool() { return; }
        let line = self.edit_line_height();
        let label = if let Some(index) = self.view_rename_index {
            self.file_view.name_rect(self.view_rect(), self.dpi, index)
        } else if let Some(index) = self.tree_rename_index {
            self.nav_tree.label_rect(self.tree_rect(), index)
        } else {
            return;
        };
        let Some(label) = label else { return };
        let area = rename_area(label, line);
        self.place_edit(self.view_rename, area);
    }

    /// Opens the rename editor over a navigation-pane row.
    ///
    /// Explorer renames in place there too. Only a real directory can be
    /// renamed: a drive root and a virtual folder (图库, 网络) keep the name the
    /// Shell gives them, so the row simply has no editor.
    pub(super) unsafe fn begin_tree_rename(&mut self, index: usize) {
        if !self.custom_tree() { return; }
        let Some(location) = self.nav_tree.location_of(index) else { return };
        let Some(path) = location.as_path() else { return };
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        if name.is_empty() { return; }
        let Some(label) = self.nav_tree.label_rect(self.tree_rect(), index) else { return };
        if !unsafe { self.ensure_view_rename_edit() } { return }
        self.view_rename_index = None;
        self.tree_rename_index = Some(index);
        self.file_view.set_editing(None);
        self.nav_tree.set_editing(Some(index));
        let line = self.edit_line_height();
        // The painted label starts at `label.x`; the editor's text starts there
        // too, so the name does not jump when the field opens.
        let area = rename_area(label, line);
        self.place_edit(self.view_rename, area);
        let text = wide(&name);
        unsafe {
            let _ = SetWindowTextW(self.view_rename, PCWSTR(text.as_ptr()));
            let _ = ShowWindow(self.view_rename, SW_SHOW);
            let _ = SetFocus(Some(self.view_rename));
            SendMessageW(self.view_rename, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
        self.invalidate();
    }

    /// Renames the folder shown by the row being edited.
    pub(super) unsafe fn commit_tree_rename(&mut self) -> bool {
        let Some(index) = self.tree_rename_index.take() else { return false };
        self.nav_tree.set_editing(None);
        let text = if self.view_rename.0.is_null() { String::new() } else { super::navigation::window_text(self.view_rename) };
        if !self.view_rename.0.is_null() { let _ = unsafe { ShowWindow(self.view_rename, SW_HIDE) }; }
        let Some(location) = self.nav_tree.location_of(index) else { return true };
        let Some(path) = location.as_path() else { return true };
        let current = self.current_location().clone();
        let name = text.trim().to_string();
        let old = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        if !name.is_empty() && name != old && !name.contains(['\\', '/']) {
            let target = match path.parent() {
                Some(parent) => parent.join(&name),
                None => PathBuf::from(&name),
            };
            if target.exists() {
                self.show_message(format!("已存在同名项目：{name}"));
            } else if let Err(error) = std::fs::rename(&path, &target) {
                self.show_message(format!("重命名失败：{error}"));
            } else if current.as_path() == Some(path) {
                // The open folder itself moved: follow the new name instead of
                // leaving the window pointing at a path that no longer exists.
                unsafe { self.navigate_to(ShellLocation::Path(target.clone())) };
            }
        }
        self.rebuild_tree();
        let _ = unsafe { SetFocus(Some(self.hwnd)) };
        true
    }

    pub(super) unsafe fn cancel_tree_rename(&mut self) -> bool {
        if self.tree_rename_index.take().is_none() { return false; }
        self.nav_tree.set_editing(None);
        if !self.view_rename.0.is_null() { let _ = unsafe { ShowWindow(self.view_rename, SW_HIDE) }; }
        let _ = unsafe { SetFocus(Some(self.hwnd)) };
        true
    }

    /// Reads the pane again after a rename moved a folder somewhere else.
    fn rebuild_tree(&mut self) {
        let current = self.current_location().clone();
        let rect = self.tree_rect();
        self.nav_tree = nav_tree::NavTree::default();
        self.nav_tree.set_viewport(rect.w as i32, rect.h as i32, self.dpi);
        self.nav_tree.reveal(&current);
        self.invalidate_rect(self.layout.tree);
    }

    /// Renames the item being edited. Returns false when no editor was open.
    pub(super) unsafe fn commit_view_rename(&mut self) -> bool {
        let Some(index) = self.view_rename_index.take() else { return unsafe { self.commit_tree_rename() } };
        self.file_view.set_editing(None);
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
        if self.view_rename_index.take().is_none() { return unsafe { self.cancel_tree_rename() } }
        self.file_view.set_editing(None);
        if !self.view_rename.0.is_null() { let _ = unsafe { ShowWindow(self.view_rename, SW_HIDE) }; }
        self.focus_view();
        true
    }
}

/// Box the in-place rename editor occupies over a painted label.
///
/// The box keeps the label's centre line, so the text the editor draws sits on
/// the same baseline as the name the row paints.
fn rename_area(label: RectF, line: i32) -> LayoutRect {
    let height = label.h.round() as i32;
    LayoutRect::new(label.x.round() as i32, label.y.round() as i32 + (height - line).max(0) / 2,
        label.w.round() as i32, line)
}

/// The system double-click interval, with a floor so a very short setting
/// still lets an ordinary double click open a row.
fn double_click_span() -> std::time::Duration {
    std::time::Duration::from_millis(unsafe { GetDoubleClickTime() }.max(500) as u64)
}

/// Token for one image request. Stable within the process, never zero, and the
/// same key always yields the same token so the worker can drop superseded work.
fn image_token(key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish() | 1
}

/// Rows a drag carries: the whole selection when the pressed row is part of it,
/// otherwise just that row. This is the rule Explorer follows, and it is what
/// keeps a drag from silently dropping the rest of a multi-selection.
fn drag_rows(selection: &std::collections::BTreeSet<usize>, index: usize) -> Vec<usize> {
    if selection.contains(&index) { selection.iter().copied().collect() } else { vec![index] }
}

#[cfg(test)]
mod drag_tests {
    use super::drag_rows;
    use std::collections::BTreeSet;

    #[test]
    fn a_pressed_row_inside_the_selection_drags_all_of_it() {
        let selection: BTreeSet<usize> = [1, 4, 7].into_iter().collect();
        assert_eq!(drag_rows(&selection, 4), vec![1, 4, 7]);
    }

    #[test]
    fn a_pressed_row_outside_the_selection_drags_only_itself() {
        let selection: BTreeSet<usize> = [1, 4].into_iter().collect();
        assert_eq!(drag_rows(&selection, 9), vec![9]);
        assert_eq!(drag_rows(&BTreeSet::new(), 3), vec![3]);
    }
}
