//! Self-drawn file list.
//!
//! The Shell still owns the folder model — enumeration, sorting data, verbs and
//! the drag/drop targets — but nothing of it is visible: ZSDesk paints the rows
//! itself so a DWM material or a wallpaper shows through the whole window.
//!
//! Selection is mirrored into the hidden Shell view after every click. Every
//! command (copy, cut, delete, rename, the system context menu, preview) keeps
//! reading the Shell view, so replacing the painter never changes what those
//! commands act on.

use std::collections::BTreeSet;

use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use zsdesk_core::model::explorer::ShellLocation;
use zsdesk_ui::theme::Palette;
use zsdesk_windows::{
    gfx::{Align, Canvas, Color, RectF, TextStyle},
    shell::{drives, folder_items::FolderEntry},
};

/// Row height and header height in DIP.
const ROW_H: f32 = 24.0;
const HEADER_H: f32 = 26.0;
const ICON: f32 = 16.0;
const PAD: f32 = 8.0;
/// The scrollbar lane on the right edge, and the thumb inside it.
const BAR_W: f32 = 14.0;
const BAR_THUMB_W: f32 = 6.0;
/// A thumb shorter than this is hard to grab with a mouse.
const BAR_MIN_THUMB: f32 = 28.0;
/// 此电脑 draws one card per drive: icon, name, capacity bar, free/total line.
const DEVICE_W: f32 = 236.0;
const DEVICE_H: f32 = 74.0;
const DEVICE_ICON: f32 = 32.0;
/// Space between the pane edge and the first card, and the group title above.
const DEVICE_MARGIN: f32 = 8.0;
const DEVICE_HEADER: f32 = 46.0;
const DEVICE_BAR_W: f32 = 168.0;
const DEVICE_BAR_H: f32 = 12.0;
/// Segoe Fluent "Page" glyph used for files without an icon.
const FILE_GLYPH: &str = "\u{E7C3}";
/// Default and minimum widths of 修改日期 / 类型 / 大小 in DIP. The name
/// column takes whatever room is left, so only these three are stored.
const DEFAULT_TAIL: [f32; 3] = [132.0, 96.0, 86.0];
const MIN_TAIL: [f32; 3] = [64.0, 56.0, 56.0];
/// The name column always keeps at least this much room, so dragging a
/// boundary can never push the file names out of the pane.
const MIN_NAME: f32 = 120.0;
/// How close to a boundary the pointer has to be to grab it, in DIP.
const DIVIDER_GRAB: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Column { Name, Modified, Kind, Size }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SortKey { Name, Modified, Kind, Size, Created }

/// How the list lays its rows out.
///
/// The Shell view offers eight view modes; the drawn list keeps the same
/// names and gives each one a geometry of its own, so 查看 → 大图标 really
/// makes the icons bigger instead of doing nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Layout {
    /// Column header and one wide row per item (详细信息).
    Details,
    /// Small icon with the name beside it, packed into columns (列表).
    Listing,
    /// Icon with two lines of text beside it (内容).
    Content,
    /// Icon above the name, laid out in a grid (every icon mode).
    Icons { icon: f32, lines: u8 },
}

impl Layout {
    /// Maps a `menus::VIEW_CHOICES` key to the drawn geometry.
    pub(super) fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "details" => Layout::Details,
            "list" => Layout::Listing,
            "content" => Layout::Content,
            "xl" => Layout::Icons { icon: 96.0, lines: 1 },
            "large" => Layout::Icons { icon: 64.0, lines: 1 },
            "medium" => Layout::Icons { icon: 48.0, lines: 1 },
            "small" => Layout::Icons { icon: 24.0, lines: 1 },
            "tiles" => Layout::Icons { icon: 48.0, lines: 2 },
            _ => return None,
        })
    }

    /// The view choice that is ticked in the menu right now.
    pub(super) fn id(self) -> &'static str {
        match self {
            Layout::Details => "details",
            Layout::Listing => "list",
            Layout::Content => "content",
            Layout::Icons { icon, lines } => match (icon as i32, lines) {
                (96.., _) => "xl",
                (64.., _) => "large",
                (48.., 2..) => "tiles",
                (48.., _) => "medium",
                _ => "small",
            },
        }
    }

    fn is_details(self) -> bool { matches!(self, Layout::Details) }

    /// Pixel size the icon is requested at, so a 96 DIP icon is not an
    /// upscaled 16 px thumbnail.
    pub(super) fn icon_px(self, dpi: u32) -> u32 {
        let icon = match self {
            Layout::Details => ICON,
            Layout::Listing => 16.0,
            Layout::Content => 32.0,
            Layout::Icons { icon, .. } => icon,
        };
        ((icon * dpi as f32 / 96.0).round() as u32).clamp(16, 256)
    }
}

/// Cell size, column count and header height of the current layout.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cells { w: f32, h: f32, cols: usize, header: f32 }

impl Column {
    pub(super) fn key(self) -> SortKey {
        match self { Column::Name => SortKey::Name, Column::Modified => SortKey::Modified, Column::Kind => SortKey::Kind, Column::Size => SortKey::Size }
    }
    pub(super) fn title(self) -> &'static str {
        match self { Column::Name => "名称", Column::Modified => "修改日期", Column::Kind => "类型", Column::Size => "大小" }
    }
    /// Slot in the resizable tail, or `None` for the name column, which fills
    /// whatever the tail leaves over.
    pub(super) fn tail(self) -> Option<usize> {
        match self { Column::Name => None, Column::Modified => Some(0), Column::Kind => Some(1), Column::Size => Some(2) }
    }
}

impl Default for Layout {
    fn default() -> Self { Layout::Details }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ViewHit {
    None,
    Header(Column),
    /// A column boundary in the header, by its slot in `DEFAULT_TAIL`.
    HeaderDivider(usize),
    Row(usize),
    Empty,
    ScrollBar { thumb: bool },
}

#[derive(Default)]
pub(super) struct FileView {
    pub items: Vec<FolderEntry>,
    /// Indices into `items`, always sorted ascending.
    pub selection: BTreeSet<usize>,
    hover: Option<usize>,
    /// Pointer anchor for shift ranges.
    anchor: Option<usize>,
    scroll: f32,
    sort: Option<SortKey>,
    ascending: bool,
    /// How the rows are drawn; also what the 查看 menu ticks.
    layout: Layout,
    /// Columns of the current layout, remembered for arrow-key stepping.
    columns: usize,
    pub location: Option<ShellLocation>,
    /// False until the folder produced a listing (or failed to).
    pub loaded: bool,
    pub error: Option<String>,
    /// Bumped by every request; a listing that comes back with an older token
    /// belongs to a folder the user has already left.
    token: u64,
    /// Pointer offset inside the thumb while it is being dragged.
    bar_drag: Option<f32>,
    /// Column boundary being dragged: (tail slot, grab x, widths at the grab).
    /// Widths are DIP, so the drag keeps its size when the window changes DPI.
    divider_drag: Option<(usize, f32, [f32; 3])>,
    /// Widths of 修改日期 / 类型 / 大小 in DIP; zero means "use the default".
    column_widths: [f32; 3],
    /// Pointer is over the scrollbar lane, so it is drawn stronger.
    bar_hot: bool,
    /// Pointer is over a column boundary, so that boundary is drawn.
    divider_hot: Option<usize>,
    /// Folder row a drag is hovering over: outlined so the target is obvious.
    drop_target: Option<usize>,
    /// Message shown instead of the empty-folder text. Search sets it, because
    /// an empty pane there means "still looking" or "nothing matched", never
    /// that the folder has no files.
    empty_message: Option<String>,
    /// Row whose name the in-place rename editor is covering. Its name is not
    /// painted, so the editor's own text is the only one on screen.
    editing: Option<usize>,
    width: i32,
    height: i32,
}

impl FileView {
    /// Starts a listing: rows of the previous folder go away at once, so the
    /// view never shows one folder's files under another folder's title.
    /// Re-listing the folder that is already shown keeps its rows and its
    /// selection until the new listing replaces them.
    pub(super) fn begin_load(&mut self, location: &ShellLocation) -> u64 {
        self.token = self.token.wrapping_add(1);
        if self.location.as_ref() != Some(location) {
            self.location = Some(location.clone());
            self.items.clear();
            self.selection.clear();
            self.anchor = None;
            self.hover = None;
            self.scroll = 0.0;
            self.error = None;
            self.loaded = false;
        }
        self.token
    }

    /// Starts a new result set for `location`, always dropping the rows already
    /// on screen. Search pages need this: the query is the same, the page is
    /// not, and `begin_load` would keep the old rows for the same location.
    pub(super) fn restart(&mut self, location: &ShellLocation) -> u64 {
        self.location = None;
        self.begin_load(location)
    }

    /// Applies a finished listing. `false` means it was superseded by a newer
    /// request and is dropped.
    pub(super) fn finish_load(&mut self, token: u64, entries: Result<Vec<FolderEntry>, String>) -> bool {
        if token != self.token { return false; }
        let keep = self.names();
        match entries {
            Ok(mut items) => {
                if let Some(key) = self.sort { sort_items(&mut items, key, self.ascending); }
                else { sort_items(&mut items, SortKey::Name, true); }
                self.items = items;
                self.error = None;
                self.loaded = true;
                self.selection.clear();
                self.anchor = None;
                self.adopt(&keep);
                true
            }
            Err(error) => {
                self.items.clear();
                self.selection.clear();
                self.error = Some(format!("无法列出这个文件夹：{error}"));
                self.loaded = false;
                true
            }
        }
    }

    /// Adds rows that arrived after the listing already on screen and keeps
    /// what the user had selected, for results that stream in page by page.
    /// The rows are re-sorted as a whole, so a growth spurt never leaves the
    /// table half in name order and half in arrival order.
    pub(super) fn append_load(&mut self, token: u64, entries: Result<Vec<FolderEntry>, String>) -> bool {
        if token != self.token { return false; }
        let Ok(entries) = entries else { return false };
        if entries.is_empty() { return false; }
        let keep = self.names();
        self.items.extend(entries);
        let key = self.sort.unwrap_or(SortKey::Name);
        sort_items(&mut self.items, key, self.ascending);
        self.loaded = true;
        self.error = None;
        self.selection.clear();
        self.anchor = None;
        self.adopt(&keep);
        true
    }

    /// Drops rows whose file no longer exists, keeping the scroll position and
    /// the selection of the rows that are left. Search results are a snapshot
    /// of a query, so a delete has to be folded in without re-running it.
    /// Returns whether anything changed.
    pub(super) fn retain_existing(&mut self) -> bool {
        let keep = self.names();
        let before = self.items.len();
        self.items.retain(|entry| entry.path.as_ref().map_or(true, |path| path.exists()));
        if self.items.len() == before { return false; }
        self.selection.clear();
        self.anchor = None;
        self.adopt(&keep);
        true
    }

    /// Text drawn when the list has no rows at all.
    pub(super) fn set_empty_message(&mut self, message: Option<String>) { self.empty_message = message; }

    fn empty_text(&self, fallback: &str) -> String {
        self.error.clone().or_else(|| self.empty_message.clone())
            .unwrap_or_else(|| if self.loaded { fallback.into() } else { "正在读取…".into() })
    }

    /// Selects the rows named by a "show this in its folder" request and
    /// reports whether the folder held every one of them. Matching accepts the
    /// drawn label as well as the real file name: the list may hide a known
    /// extension, while the request names the file the Shell knows.
    pub(super) fn select_names(&mut self, names: &[String]) -> bool {
        if names.is_empty() { return false; }
        let mut found = Vec::with_capacity(names.len());
        for name in names {
            let index = self.items.iter().position(|entry| entry.name.eq_ignore_ascii_case(name)
                || entry.file_name().eq_ignore_ascii_case(name));
            match index { Some(index) => found.push(index), None => return false }
        }
        self.selection = found.into_iter().collect();
        self.anchor = self.selection.iter().next_back().copied();
        self.hover = None;
        true
    }

    /// Applies a sort the user picked in the 排序 menu. A key already in use
    /// flips the direction, exactly like clicking the column header.
    pub(super) fn set_sort(&mut self, key: SortKey) {
        if self.sort == Some(key) { self.ascending = !self.ascending; } else { self.sort = Some(key); self.ascending = true; }
        self.apply_sort();
    }

    /// Applies an explicit key and direction (排序 → 递增 / 递减).
    pub(super) fn sort_by(&mut self, key: SortKey, ascending: bool) {
        self.sort = Some(key);
        self.ascending = ascending;
        self.apply_sort();
    }

    /// Sort keys survive a reload; the rows are re-ordered and the scroll
    /// position returns to the top so the first row is where it is expected.
    /// The selection follows its items by name, so sorting does not silently
    /// drop what the user had picked.
    fn apply_sort(&mut self) {
        let keep = self.names();
        if let Some(key) = self.sort { sort_items(&mut self.items, key, self.ascending); }
        self.selection.clear();
        self.anchor = None;
        self.scroll = 0.0;
        self.adopt(&keep);
        if let Some(index) = self.focus() { self.anchor = Some(index); }
    }

    pub(super) fn sort_state(&self, key: SortKey) -> Option<bool> {
        (self.sort == Some(key)).then_some(self.ascending)
    }

    /// The sort the 排序 menu ticks.
    pub(super) fn sort_order(&self) -> Option<(SortKey, bool)> { self.sort.map(|key| (key, self.ascending)) }

    /// The layout the 查看 menu ticks.
    pub(super) fn layout(&self) -> Layout { self.layout }

    /// Switches the layout. The selection stays; the view returns to the top
    /// so the first cell of the new grid is what the user sees.
    pub(super) fn set_layout(&mut self, layout: Layout) -> bool {
        if self.layout == layout { return false; }
        self.layout = layout;
        self.scroll = 0.0;
        true
    }

    fn row_height(&self, dpi: u32) -> f32 { ROW_H * dpi as f32 / 96.0 }
    fn header_height(&self, dpi: u32) -> f32 { HEADER_H * dpi as f32 / 96.0 }

    /// Cell size, column count and header height for a pane of `rect`.
    ///
    /// Grid layouts always reserve the scrollbar lane: the column count then
    /// stays put when the bar appears, instead of re-flowing the whole grid.
    fn cells(&self, rect: RectF, dpi: u32) -> Cells {
        let s = |value: f32| value * dpi as f32 / 96.0;
        if self.is_devices() {
            // 此电脑 is a board of device cards under one group title, not a
            // list of files: every card carries its own capacity bar.
            let width = (rect.w - s(DEVICE_MARGIN) - s(BAR_W)).max(s(DEVICE_W));
            let cols = (width / s(DEVICE_W)).floor().max(1.0) as usize;
            return Cells { w: s(DEVICE_W), h: s(DEVICE_H), cols, header: s(DEVICE_HEADER) };
        }
        if self.layout.is_details() {
            return Cells { w: rect.w, h: self.row_height(dpi), cols: 1, header: self.header_height(dpi) };
        }
        let width = (rect.w - s(BAR_W)).max(1.0);
        let (w, h) = match self.layout {
            Layout::Listing => (s(240.0), s(22.0)),
            Layout::Content => (s(210.0), s(46.0)),
            Layout::Icons { icon, lines } => {
                let labels = s(18.0) * lines.max(1) as f32;
                // A second line repeats the type and the size, so the cell has
                // to be wide enough to show them instead of an ellipsis.
                let width = if lines > 1 { s(icon) + s(96.0) } else { s(icon) + s(28.0) };
                (width.max(s(88.0)), s(icon) + s(10.0) + labels + s(6.0))
            }
            Layout::Details => (rect.w, self.row_height(dpi)),
        };
        Cells { w, h, cols: ((width / w).floor() as usize).max(1), header: 0.0 }
    }

    /// Total height of every row, which is what the scrollbar measures.
    fn total_height(&self, rect: RectF, dpi: u32) -> f32 {
        let cells = self.cells(rect, dpi);
        let rows = self.items.len().div_ceil(cells.cols.max(1));
        rows as f32 * cells.h
    }

    /// Rectangle of one cell in window coordinates.
    fn cell_rect(&self, rect: RectF, dpi: u32, index: usize) -> RectF {
        let cells = self.cells(rect, dpi);
        let content = self.content_rect(rect, dpi);
        let row = index / cells.cols.max(1);
        let column = index % cells.cols.max(1);
        RectF::new(content.x + column as f32 * cells.w, content.y + cells.header + row as f32 * cells.h - self.scroll, cells.w, cells.h)
    }

    /// Row height of the current layout: one wheel notch scrolls three of these.
    pub(super) fn scroll_step(&self, dpi: u32) -> f32 {
        let rect = RectF::new(0.0, 0.0, self.width as f32, self.height as f32);
        self.cells(rect, dpi).h
    }

    /// Rows a Page Up / Page Down moves in the current layout.
    pub(super) fn page_rows(&self, dpi: u32) -> isize {
        let rect = RectF::new(0.0, 0.0, self.width as f32, self.height as f32);
        let cells = self.cells(rect, dpi);
        let body = (self.height as f32 - cells.header).max(cells.h);
        ((body / cells.h.max(1.0)).floor() as isize - 1).max(1)
    }

    /// Width of a tail column in DIP, with the default standing in for a
    /// session that never stored one.
    fn width_dip(&self, index: usize) -> f32 {
        let stored = self.column_widths.get(index).copied().unwrap_or(0.0);
        if stored.is_finite() && stored > 0.0 { stored } else { DEFAULT_TAIL[index] }
    }

    /// Stores the widths the user dragged. Values that are missing or out of
    /// range are ignored so they fall back to the defaults instead of
    /// collapsing a column to zero.
    pub(super) fn set_column_widths(&mut self, dips: [i32; 3]) {
        for (index, dip) in dips.into_iter().enumerate() {
            if dip > 0 { self.column_widths[index] = dip as f32; }
        }
    }

    /// The widths to persist: what the table is drawn with right now.
    pub(super) fn column_widths_dip(&self) -> [i32; 3] {
        [self.width_dip(0).round() as i32, self.width_dip(1).round() as i32, self.width_dip(2).round() as i32]
    }

    /// Column rectangles for the current viewport; the name column takes the rest.
    ///
    /// Every boundary follows the pointer because only the column on its right
    /// changes width: the name column absorbs the difference, which is the
    /// column Explorer lets a details view stretch.
    pub(super) fn columns(&self, rect: RectF, dpi: u32) -> Vec<(Column, RectF)> {
        let s = |value: f32| value * dpi as f32 / 96.0;
        let tail = [Column::Modified, Column::Kind, Column::Size];
        let mut widths = [s(self.width_dip(0)), s(self.width_dip(1)), s(self.width_dip(2))];
        // A pane narrower than the columns ask for squeezes them instead of
        // cutting the last one off; the name keeps its minimum.
        let used: f32 = widths.iter().sum();
        let room = rect.w - s(MIN_NAME);
        if used > room && used > 0.0 {
            let factor = (room.max(0.0) / used).clamp(0.0, 1.0);
            for width in &mut widths { *width *= factor; }
        }
        let used: f32 = widths.iter().sum();
        let name = (rect.w - used).max(0.0);
        let mut result = vec![(Column::Name, RectF::new(rect.x, rect.y, name, rect.h))];
        let mut x = rect.x + name;
        for (column, width) in tail.into_iter().zip(widths) {
            result.push((column, RectF::new(x, rect.y, width, rect.h)));
            x += width;
        }
        result
    }

    /// Column boundary under `x`, as the slot of the column on its right.
    ///
    /// Only the header band carries boundaries: the rows themselves are a
    /// click target, and Explorer resizes from the same place.
    /// Column boundary under `x`, as the slot of the column on its right.
    ///
    /// `content` is the pane without the scrollbar lane, which is what the
    /// header and the rows are laid out in, so a boundary sits exactly where
    /// the column edge is drawn. Only the header band carries boundaries: the
    /// rows themselves are a click target.
    fn divider_at(&self, content: RectF, dpi: u32, x: f32) -> Option<usize> {
        if self.is_devices() || !self.layout.is_details() { return None; }
        let s = |value: f32| value * dpi as f32 / 96.0;
        let header = RectF::new(content.x, content.y, content.w, self.header_height(dpi));
        let grab = s(DIVIDER_GRAB).max(2.0);
        // The boundary belongs to the column on its right, which is the one a
        // drag resizes; the name column has no tail slot of its own.
        self.columns(header, dpi).into_iter()
            .find_map(|(column, area)| ((x - area.x).abs() <= grab).then_some(column.tail()).flatten())
    }

    /// Grabs the boundary under `x`. True means a resize started and the
    /// caller should capture the pointer.
    pub(super) fn press_divider(&mut self, rect: RectF, dpi: u32, x: f32) -> bool {
        let content = self.content_rect(rect, dpi);
        let Some(index) = self.divider_at(content, dpi, x) else { return false };
        self.divider_drag = Some((index, x, [self.width_dip(0), self.width_dip(1), self.width_dip(2)]));
        true
    }

    /// Follows the pointer: the grabbed column resizes from the boundary, so a
    /// drag to the right makes the column on the right narrower.
    pub(super) fn drag_divider(&mut self, rect: RectF, dpi: u32, x: f32) {
        let Some((index, start_x, start)) = self.divider_drag else { return };
        let dip = dpi as f32 / 96.0;
        let mut widths = start;
        widths[index] -= (x - start_x) / dip;
        // The column keeps a usable width, and the name column keeps its room:
        // a boundary never drags the table wider than the pane.
        let others: f32 = widths.iter().enumerate().filter(|(slot, _)| *slot != index).map(|(_, width)| *width).sum();
        let room = self.content_rect(rect, dpi).w / dip - MIN_NAME - others;
        widths[index] = widths[index].clamp(MIN_TAIL[index], room.max(MIN_TAIL[index]));
        self.column_widths = widths;
    }

    pub(super) fn dragging_divider(&self) -> bool { self.divider_drag.is_some() }

    /// Pointer entered or left a column boundary; that rule is drawn stronger.
    pub(super) fn set_divider_hot(&mut self, index: Option<usize>) -> bool {
        if self.divider_hot == index { return false; }
        self.divider_hot = index;
        true
    }

    pub(super) fn end_divider_drag(&mut self) -> bool { self.divider_drag.take().is_some() }


    pub(super) fn visible_rows(&self, rect: RectF, dpi: u32) -> std::ops::Range<usize> {
        let cells = self.cells(rect, dpi);
        if cells.h <= 0.0 { return 0..0; }
        let body = (rect.h - cells.header).max(0.0);
        let first_row = (self.scroll / cells.h).floor().max(0.0) as usize;
        let rows = (body / cells.h).ceil() as usize + 1;
        let first = (first_row * cells.cols).min(self.items.len());
        let last = ((first_row + rows) * cells.cols).min(self.items.len());
        first..last
    }

    /// True while 此电脑 is on screen: the rows are drives, drawn as cards.
    pub(super) fn is_devices(&self) -> bool { matches!(self.location, Some(ShellLocation::ThisPc)) }

    /// Pixel size the icon is requested at for the view on screen.
    pub(super) fn icon_px(&self, dpi: u32) -> u32 {
        if self.is_devices() { return ((DEVICE_ICON * dpi as f32 / 96.0).round() as u32).clamp(16, 256); }
        self.layout.icon_px(dpi)
    }

    pub(super) fn hit(&self, rect: RectF, dpi: u32, x: i32, y: i32) -> ViewHit {
        if !rect.contains(x as f32, y as f32) { return ViewHit::None; }
        if let Some((track, thumb)) = self.scrollbar(rect, dpi) {
            if (x as f32) >= track.x && (y as f32) >= track.y {
                return ViewHit::ScrollBar { thumb: thumb.contains(x as f32, y as f32) };
            }
        }
        let cells = self.cells(rect, dpi);
        let rect = self.content_rect(rect, dpi);
        if !self.layout.is_details() || self.is_devices() {
            let local_x = x as f32 - rect.x;
            // The group title and the gap around the cards are not cells.
            let local_y = y as f32 - rect.y - cells.header + self.scroll;
            if local_x < 0.0 || local_y < 0.0 { return ViewHit::Empty; }
            let column = (local_x / cells.w) as usize;
            if column >= cells.cols { return ViewHit::Empty; }
            let index = (local_y / cells.h) as usize * cells.cols + column;
            return if index < self.items.len() { ViewHit::Row(index) } else { ViewHit::Empty };
        }
        let header = cells.header;
        let row = cells.h;
        if (y as f32) < rect.y + header {
            // A boundary is grabbed before the column under it, so pressing
            // there resizes the table instead of sorting it.
            if let Some(index) = self.divider_at(rect, dpi, x as f32) { return ViewHit::HeaderDivider(index); }
            let columns = self.columns(RectF::new(rect.x, rect.y, rect.w, header), dpi);
            for (column, area) in &columns {
                if (x as f32) >= area.x && (x as f32) < area.right() { return ViewHit::Header(*column); }
            }
            // Right of the table — the scrollbar lane — still belongs to the
            // last column, so a click there sorts by 大小 rather than 名称.
            return ViewHit::Header(columns.last().map(|(column, _)| *column).unwrap_or(Column::Name));
        }
        let offset = (y as f32) - rect.y - header + self.scroll;
        let index = (offset / row).floor();
        if index < 0.0 { return ViewHit::Empty; }
        let index = index as usize;
        if index < self.items.len() { ViewHit::Row(index) } else { ViewHit::Empty }
    }

    /// Applies a click and returns the names to mirror into the Shell view.
    pub(super) fn click(&mut self, index: Option<usize>, control: bool, shift: bool) -> Vec<String> {
        self.selection = match (index, control, shift) {
            (Some(index), true, _) => {
                let mut selection = self.selection.clone();
                if !selection.insert(index) { selection.remove(&index); }
                self.anchor = Some(index);
                selection
            }
            (Some(index), false, true) => {
                let anchor = self.anchor.unwrap_or(index).min(self.items.len().saturating_sub(1));
                let (low, high) = if anchor <= index { (anchor, index) } else { (index, anchor) };
                (low..=high).collect()
            }
            (Some(index), false, false) => {
                self.anchor = Some(index);
                [index].into_iter().collect()
            }
            (None, _, _) => { self.anchor = None; BTreeSet::new() }
        };
        self.file_names()
    }

    /// Names of the selected entries, in listing order.
    pub(super) fn names(&self) -> Vec<String> {
        self.selection.iter().filter_map(|index| self.items.get(*index)).map(|entry| entry.name.clone()).collect()
    }

    /// Shell identities of the rows at `indices`: what a drag hands to OLE, and
    /// what a menu built from the drawn selection is made of. A parsing name
    /// works for virtual items as well as real files.
    pub(super) fn parsing_names(&self, indices: &[usize]) -> Vec<String> {
        indices.iter().filter_map(|index| self.items.get(*index)).map(|entry| entry.parsing_name.clone()).collect()
    }

    /// Real file names of the selection: what the Shell parses without guessing
    /// at a localized or extension-less display name.
    pub(super) fn file_names(&self) -> Vec<String> {
        self.selection.iter().filter_map(|index| self.items.get(*index)).map(FolderEntry::file_name).collect()
    }

    /// Total size of the selected files, for the status bar. A selected drive
    /// counts with the whole volume, which is what Explorer reports.
    pub(super) fn selected_bytes(&self) -> Option<u64> {
        if self.selection.is_empty() { return None; }
        let mut total = 0u64;
        for index in &self.selection {
            let size = self.items.get(*index)
                .and_then(|entry| entry.size.or_else(|| entry.drive.map(|drive| drive.total)))
                .unwrap_or(0);
            total = total.checked_add(size)?;
        }
        Some(total)
    }

    /// Replaces the selection from names reported by the Shell view.
    pub(super) fn adopt(&mut self, names: &[String]) {
        self.selection.clear();
        if names.is_empty() { return; }
        for (index, entry) in self.items.iter().enumerate() {
            // A drive answers to its root ("D:\") and to its display name.
            let root = entry.file_name();
            if names.iter().any(|name| same_name(&entry.name, name) || root.eq_ignore_ascii_case(name)) {
                self.selection.insert(index);
            }
        }
    }

    pub(super) fn set_hover(&mut self, hover: Option<usize>) -> bool {
        if self.hover == hover { return false; }
        self.hover = hover;
        true
    }

    /// Moves the selection by `delta` rows (arrows, page keys, home/end). In a
    /// grid a "row" is a line of cells, so one step moves a whole row across.
    pub(super) fn step(&mut self, delta: isize, extend: bool) {
        if self.items.is_empty() { return; }
        let last = self.items.len() - 1;
        let current = self.selection.iter().next_back().copied().unwrap_or(0);
        let stride = if self.layout.is_details() { 1 } else { self.columns.max(1) } as isize;
        let next = (current as isize + delta.saturating_mul(stride)).clamp(0, last as isize) as usize;
        if extend {
            let anchor = self.anchor.unwrap_or(current);
            let (low, high) = if anchor <= next { (anchor, next) } else { (next, anchor) };
            self.selection = (low..=high).collect();
        } else {
            self.selection = [next].into_iter().collect();
            self.anchor = Some(next);
        }
    }

    pub(super) fn select_all(&mut self) { self.selection = (0..self.items.len()).collect(); }

    pub(super) fn focus(&self) -> Option<usize> { self.selection.iter().next_back().copied() }

    pub(super) fn set_viewport(&mut self, rect: RectF, dpi: u32) {
        self.width = rect.w as i32;
        self.height = rect.h as i32;
        self.columns = self.cells(rect, dpi).cols;
        self.clamp_scroll(dpi);
    }

    pub(super) fn scroll_by(&mut self, delta: f32, dpi: u32) {
        self.scroll += delta;
        self.clamp_scroll(dpi);
    }

    /// Scrolls the focused row into view.
    pub(super) fn reveal_focus(&mut self, dpi: u32) {
        let Some(index) = self.focus() else { return };
        let rect = RectF::new(0.0, 0.0, self.width as f32, self.height as f32);
        let cells = self.cells(rect, dpi);
        let row = (index / cells.cols.max(1)) as f32;
        let top = row * cells.h;
        let bottom = top + cells.h;
        let viewport = (self.height as f32 - cells.header).max(cells.h);
        if top < self.scroll { self.scroll = top; }
        else if bottom > self.scroll + viewport { self.scroll = bottom - viewport; }
        self.clamp_scroll(dpi);
    }

    fn clamp_scroll(&mut self, dpi: u32) {
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(dpi));
    }

    /// How far the list can scroll; zero while every row fits.
    pub(super) fn max_scroll(&self, dpi: u32) -> f32 {
        let rect = RectF::new(0.0, 0.0, self.width as f32, self.height as f32);
        self.max_scroll_in(rect, dpi)
    }

    /// Same, for a pane the caller is drawing into right now.
    fn max_scroll_in(&self, rect: RectF, dpi: u32) -> f32 {
        let cells = self.cells(rect, dpi);
        (self.total_height(rect, dpi) - (rect.h - cells.header).max(0.0)).max(0.0)
    }

    /// Scrollbar lane and thumb, or `None` when the rows all fit. The lane
    /// starts under the column header so the header keeps its full width.
    pub(super) fn scrollbar(&self, rect: RectF, dpi: u32) -> Option<(RectF, RectF)> {
        let s = |value: f32| value * dpi as f32 / 96.0;
        let max = self.max_scroll_in(rect, dpi);
        let header = self.cells(rect, dpi).header;
        if max <= 0.0 || rect.h <= header { return None; }
        let track = RectF::new(rect.right() - s(BAR_W), rect.y + header, s(BAR_W), rect.h - header);
        let content = self.total_height(rect, dpi);
        let thumb_h = (track.h * (track.h / content.max(1.0))).clamp(s(BAR_MIN_THUMB), track.h);
        let travel = (track.h - thumb_h).max(0.0);
        let y = track.y + (self.scroll / max) * travel;
        let thumb = RectF::new(track.x + (s(BAR_W) - s(BAR_THUMB_W)) / 2.0, y, s(BAR_THUMB_W), thumb_h);
        Some((track, thumb))
    }

    /// The pane without the scrollbar lane: where rows and columns live, so a
    /// long name never runs under the thumb. Grid layouts always give the lane
    /// away, so the column count does not change when the bar appears.
    pub(super) fn content_rect(&self, rect: RectF, dpi: u32) -> RectF {
        let s = |value: f32| value * dpi as f32 / 96.0;
        if self.is_devices() {
            // Cards keep a margin from the pane edge and never run under the bar.
            return RectF::new(rect.x + s(DEVICE_MARGIN), rect.y,
                (rect.w - s(DEVICE_MARGIN) - s(BAR_W)).max(0.0), rect.h);
        }
        if !self.layout.is_details() {
            return RectF::new(rect.x, rect.y, (rect.w - s(BAR_W)).max(0.0), rect.h);
        }
        match self.scrollbar(rect, dpi) {
            Some((track, _)) => RectF::new(rect.x, rect.y, (track.x - rect.x).max(0.0), rect.h),
            None => rect,
        }
    }

    /// Thumb pressed: remembers where inside it the pointer landed. Clicking
    /// the lane above or below pages instead, like Explorer.
    pub(super) fn press_bar(&mut self, rect: RectF, dpi: u32, y: f32, on_thumb: bool) -> bool {
        let Some((track, thumb)) = self.scrollbar(rect, dpi) else { return false };
        if on_thumb {
            self.bar_drag = Some((y - thumb.y).clamp(0.0, thumb.h));
            return true;
        }
        let page = (track.h - self.cells(rect, dpi).h).max(self.cells(rect, dpi).h);
        self.scroll += if y < thumb.y { -page } else { page };
        self.clamp_scroll(dpi);
        false
    }

    /// Drags the thumb: the pointer keeps the offset it grabbed it by.
    pub(super) fn drag_bar(&mut self, rect: RectF, dpi: u32, y: f32) {
        let Some(grab) = self.bar_drag else { return };
        let Some((track, thumb)) = self.scrollbar(rect, dpi) else { return };
        let travel = (track.h - thumb.h).max(0.0);
        if travel <= 0.0 { return; }
        let ratio = ((y - grab - track.y) / travel).clamp(0.0, 1.0);
        self.scroll = ratio * self.max_scroll_in(rect, dpi);
        self.clamp_scroll(dpi);
    }

    pub(super) fn dragging_bar(&self) -> bool { self.bar_drag.is_some() }

    pub(super) fn end_bar_drag(&mut self) -> bool { self.bar_drag.take().is_some() }

    /// Pointer entered or left the lane; the thumb gets a stronger tint.
    pub(super) fn set_bar_hot(&mut self, hot: bool) -> bool {
        if self.bar_hot == hot { return false; }
        self.bar_hot = hot;
        true
    }

    /// Row a drag would drop into; `None` clears the outline.
    pub(super) fn set_drop_target(&mut self, row: Option<usize>) -> bool {
        if self.drop_target == row { return false; }
        self.drop_target = row;
        true
    }

    /// Cell rectangle in window coordinates.
    pub(super) fn row_rect(&self, rect: RectF, dpi: u32, index: usize) -> RectF {
        self.cell_rect(rect, dpi, index)
    }

    /// Rectangle the name of `index` is painted into.
    ///
    /// The in-place rename editor is placed on exactly this rectangle, so its
    /// text lands on the letters the row already shows instead of a guessed
    /// offset — the previous `+26` guess drifted left of the name in details
    /// view and away from the centred labels of the icon layouts.
    pub(super) fn name_rect(&self, rect: RectF, dpi: u32, index: usize) -> Option<RectF> {
        if self.is_devices() { return None; }
        let cell = self.cell_rect(rect, dpi, index);
        if self.layout.is_details() {
            let s = |value: f32| value * dpi as f32 / 96.0;
            let content = self.content_rect(rect, dpi);
            let name = self.columns(RectF::new(content.x, cell.y, content.w, cell.h), dpi).into_iter().next()?.1;
            let left = name_left(cell, s);
            return Some(RectF::new(left, cell.y, (name.right() - left - s(6.0)).max(0.0), cell.h));
        }
        self.cell_text(cell, dpi).1.into_iter().next()
    }

    /// True when the name of `index` is centred inside its cell (every icon
    /// layout), which is how the editor knows to centre its own text too.
    pub(super) fn centers_names(&self) -> bool {
        matches!(self.layout, Layout::Icons { .. })
    }

    /// Marks (or clears) the row covered by the rename editor.
    pub(super) fn set_editing(&mut self, index: Option<usize>) {
        self.editing = index;
    }

    /// Icon rectangle and text rectangles of one cell.
    ///
    /// Painting and the rename editor both read this, so a layout change can
    /// never leave the editor behind on the old geometry.
    fn cell_text(&self, cell: RectF, dpi: u32) -> (RectF, Vec<RectF>) {
        let s = |value: f32| value * dpi as f32 / 96.0;
        let label = s(18.0);
        match self.layout {
            Layout::Listing => {
                let icon = RectF::new(cell.x + s(7.0), cell.y + (cell.h - s(16.0)) / 2.0, s(16.0), s(16.0));
                (icon, vec![RectF::new(icon.right() + s(7.0), cell.y, (cell.right() - icon.right() - s(12.0)).max(0.0), cell.h)])
            }
            Layout::Content => {
                let icon = RectF::new(cell.x + s(9.0), cell.y + (cell.h - s(32.0)) / 2.0, s(32.0), s(32.0));
                let left = icon.right() + s(8.0);
                let width = (cell.right() - left - s(8.0)).max(0.0);
                let top = cell.y + s(5.0);
                (icon, vec![
                    RectF::new(left, top, width, s(18.0)),
                    RectF::new(left, top + s(18.0), width, s(18.0)),
                ])
            }
            Layout::Icons { icon, lines } => {
                let size = s(icon);
                let icon_rect = RectF::new(cell.x + (cell.w - size) / 2.0, cell.y + s(6.0), size, size);
                let top = icon_rect.bottom() + s(4.0);
                let mut rects = vec![RectF::new(cell.x + s(4.0), top, (cell.w - s(8.0)).max(0.0), label)];
                if lines > 1 { rects.push(RectF::new(cell.x + s(4.0), top + label, (cell.w - s(8.0)).max(0.0), label)); }
                (icon_rect, rects)
            }
            Layout::Details => (RectF::new(cell.x, cell.y, cell.w, cell.h), Vec::new()),
        }
    }

    /// Draws header, rows, selection, the scrollbar and the empty state.
    pub(super) fn paint(&self, cv: &Canvas, rect: RectF, dpi: u32, palette: &Palette, icons: &[Option<ID2D1Bitmap>]) {
        let s = |value: f32| value * dpi as f32 / 96.0;
        if rect.w < s(40.0) || rect.h < s(20.0) { return; }
        if self.is_devices() {
            self.paint_devices(cv, rect, dpi, palette, icons);
            return;
        }
        if !self.layout.is_details() {
            self.paint_cells(cv, rect, dpi, palette, icons);
            return;
        }
        let text = TextField { palette, dpi };
        let header = self.header_height(dpi);
        let header_rect = RectF::new(rect.x, rect.y, rect.w, header);
        // The header sits on its own translucent slab, like Explorer's column bar.
        cv.fill(header_rect, text.color(palette.hover));
        // A long folder keeps its own lane on the right, so no cell runs under
        // the thumb and grabbing the bar never selects a row by accident.
        let content = self.content_rect(rect, dpi);
        cv.push_clip(rect);
        let columns = self.columns(RectF::new(content.x, content.y, content.w, header), dpi);
        // The header prints no rule of its own, so the boundaries are drawn as
        // hairlines: without them there is nothing to aim at when a column has
        // to be sized. The one under the pointer, or the one being dragged, is
        // drawn in the accent colour; the rest stay almost invisible.
        let rule = Color::rgba8(palette.divider.r, palette.divider.g, palette.divider.b, palette.divider.a);
        let hot = self.divider_drag.map(|(index, _, _)| index).or(self.divider_hot);
        for (slot, (_, area)) in columns.iter().enumerate().skip(1) {
            if area.x <= content.x { continue; }
            let lit = hot == Some(slot - 1);
            cv.fill(RectF::new(area.x - 0.5, header_rect.y, 1.0, header_rect.h),
                if lit { Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0xFF) } else { rule });
        }
        for (column, area) in self.columns(RectF::new(content.x, content.y, content.w, header), dpi) {
            let sort = self.sort_state(column.key());
            let label = match sort { Some(ascending) => format!("{} {}", column.title(), if ascending { "\u{2191}" } else { "\u{2193}" }), None => column.title().to_string() };
            let inner = RectF::new(area.x + if column == Column::Name { s(PAD) } else { s(6.0) }, area.y, (area.w - s(PAD)).max(0.0), area.h);
            cv.text(&label, inner, &TextStyle::ui(s(12.0)), text.color(if sort.is_some() { palette.text } else { palette.text_secondary }));
        }
        let first = self.visible_rows(rect, dpi).start;
        // Dragging the thumb lands on arbitrary offsets, so the top row is
        // usually half scrolled away: clip the body or its text bleeds into
        // the column header.
        let body = RectF::new(rect.x, rect.y + header, rect.w, (rect.h - header).max(0.0));
        cv.push_clip(body);
        for index in self.visible_rows(rect, dpi) {
            let row = self.row_rect(rect, dpi, index);
            if row.bottom() <= rect.y + header || row.y >= rect.bottom() { continue; }
            let selected = self.selection.contains(&index);
            let hovered = self.hover == Some(index);
            let targeted = self.drop_target == Some(index);
            let chip = RectF::new(row.x + s(2.0), row.y + 0.5, (row.w - s(4.0)).max(0.0), (row.h - 1.0).max(1.0));
            if targeted {
                // A drop target reads as a frame, not as a selection: filling it
                // like a selected row would hide that the click did not happen.
                cv.round(chip, s(4.0), zsdesk_windows::gfx::Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0x2e));
                cv.stroke_round(chip, s(4.0), zsdesk_windows::gfx::Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0xB0), s(1.0));
            } else if selected {
                cv.round(chip, s(4.0), text.accent(palette.accent));
            } else if hovered {
                cv.round(chip, s(4.0), text.color(palette.hover));
            }
            let color = if selected { text.color(palette.text) } else { text.color(palette.text) };
            let Some(entry) = self.items.get(index) else { continue };
            let icon_rect = RectF::new(row.x + s(PAD), row.y + (row.h - s(ICON)) / 2.0, s(ICON), s(ICON));
            match icons.get(index - first) {
                Some(Some(bitmap)) => cv.image(bitmap, fitted(icon_rect, bitmap), 1.0),
                _ => {
                    let glyph = if entry.is_dir { zsdesk_ui::theme::glyph::FOLDER } else { FILE_GLYPH };
                    cv.glyph(glyph, icon_rect, s(14.0), color);
                }
            }
            let columns = self.columns(RectF::new(content.x, content.y, content.w, row.h), dpi);
            for (column, area) in columns {
                let area = RectF::new(area.x, row.y, area.w, row.h);
                match column {
                    Column::Name => {
                        let left = name_left(row, s);
                        if self.editing != Some(index) {
                            cv.text(&entry.name, RectF::new(left, row.y, (area.right() - left - s(6.0)).max(0.0), row.h),
                                &TextStyle::ui(s(12.5)), color);
                        }
                    }
                    Column::Modified => cv.text(&modified_text(entry.modified), RectF::new(area.x + s(6.0), row.y, (area.w - s(10.0)).max(0.0), row.h),
                        &TextStyle::ui(s(12.0)), text.color(palette.text_secondary)),
                    Column::Kind => cv.text(&entry.kind(), RectF::new(area.x + s(6.0), row.y, (area.w - s(10.0)).max(0.0), row.h),
                        &TextStyle::ui(s(12.0)), text.color(palette.text_secondary)),
                    Column::Size => cv.text(&size_text(entry.size, entry.is_dir),
                        RectF::new(area.x, row.y, (area.w - s(10.0)).max(0.0), row.h),
                        &TextStyle::ui(s(12.0)).align(Align::End), text.color(palette.text_secondary)),
                }
            }
        }
        // Empty folder, unreadable folder or a still-loading one.
        if self.items.is_empty() {
            let message = self.empty_text("此文件夹为空");
            let style = TextStyle::ui(s(12.5));
            cv.text(&message, RectF::new(body.x, body.y, body.w, body.h),
                &style.align(Align::Center), text.color(palette.text_secondary));
        }
        cv.pop_clip();
        cv.pop_clip();
        self.paint_scrollbar(cv, rect, dpi, palette);
    }

    /// 此电脑: 「设备和驱动器」 above a board of device cards.
    ///
    /// Explorer does not list drives as files here — each one is a card with
    /// its volume icon, its name, a capacity bar and the "xx 可用，共 xx" line
    /// underneath. The geometry comes from the same `cells` the hit test uses,
    /// so a click always lands on the card it looks like.
    fn paint_devices(&self, cv: &Canvas, rect: RectF, dpi: u32, palette: &Palette, icons: &[Option<ID2D1Bitmap>]) {
        let s = |value: f32| value * dpi as f32 / 96.0;
        let text = TextField { palette, dpi };
        let dark = palette.mode == zsdesk_ui::theme::Mode::Dark;
        let gutter = if dark { Color::rgba8(0xFF, 0xFF, 0xFF, 0x29) } else { Color::rgba8(0x00, 0x00, 0x00, 0x1c) };
        let accent = Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0xFF);
        let first = self.visible_rows(rect, dpi).start;
        cv.push_clip(rect);
        cv.text("设备和驱动器", RectF::new(rect.x + s(14.0), rect.y + s(10.0), (rect.w - s(28.0)).max(0.0), s(26.0)),
            &TextStyle::ui(s(12.5)).weight(600), text.color(palette.text));
        for index in self.visible_rows(rect, dpi) {
            let cell = self.cell_rect(rect, dpi, index);
            if cell.bottom() <= rect.y + s(DEVICE_HEADER) || cell.y >= rect.bottom() { continue; }
            let selected = self.selection.contains(&index);
            let hovered = self.hover == Some(index);
            let targeted = self.drop_target == Some(index);
            let chip = RectF::new(cell.x, cell.y + s(2.0), (cell.w - s(8.0)).max(0.0), (cell.h - s(6.0)).max(1.0));
            if targeted {
                cv.round(chip, s(5.0), Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0x2e));
                cv.stroke_round(chip, s(5.0), Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0xB0), s(1.0));
            } else if selected {
                cv.round(chip, s(5.0), text.accent(palette.accent));
            } else if hovered {
                cv.round(chip, s(5.0), text.color(palette.hover));
            }
            let Some(entry) = self.items.get(index) else { continue };
            let color = text.color(palette.text);
            let icon_rect = RectF::new(cell.x + s(6.0), cell.y + s(10.0), s(DEVICE_ICON), s(DEVICE_ICON));
            match icons.get(index - first) {
                Some(Some(bitmap)) => cv.image(bitmap, fitted(icon_rect, bitmap), 1.0),
                _ => cv.glyph(zsdesk_ui::theme::glyph::FOLDER, icon_rect, s(24.0), color),
            }
            let left = icon_rect.right() + s(12.0);
            let width = (cell.right() - left - s(10.0)).max(s(60.0));
            cv.text(&entry.name, RectF::new(left, cell.y + s(6.0), width, s(20.0)),
                &TextStyle::ui(s(12.5)), color);
            let Some(drive) = entry.drive else { continue };
            if drive.total == 0 {
                // An unready device (empty card reader, open tray) keeps its
                // card, its icon and its letter, but has nothing to measure.
                cv.text(&format!("{}（未就绪）", drive.kind.label()),
                    RectF::new(left, cell.y + s(30.0), width, s(18.0)),
                    &TextStyle::ui(s(11.5)), text.color(palette.text_secondary));
                continue;
            }
            let bar = RectF::new(left, cell.y + s(30.0), width.min(s(DEVICE_BAR_W)), s(DEVICE_BAR_H));
            cv.round(bar, s(2.0), gutter);
            let filled = (bar.w * drive.fill()).clamp(0.0, bar.w);
            if filled > 0.0 {
                // Explorer leaves a two-pixel gap before the bar's end, so the
                // track stays visible even on a nearly full volume.
                let drawn = if filled < bar.w { (filled - s(2.0)).max(s(1.0)) } else { filled };
                cv.round(RectF::new(bar.x, bar.y, drawn, bar.h), s(2.0), accent);
            }
            cv.text(&format!("{} 可用，共 {}", drives::short_size(drive.free), drives::short_size(drive.total)),
                RectF::new(left, cell.y + s(46.0), width, s(18.0)),
                &TextStyle::ui(s(11.5)), text.color(palette.text_secondary));
        }
        if self.items.is_empty() {
            let message = self.empty_text("没有可显示的驱动器");
            let body = RectF::new(rect.x, rect.y + s(DEVICE_HEADER), rect.w, (rect.h - s(DEVICE_HEADER)).max(0.0));
            cv.text(&message, body, &TextStyle::ui(s(12.5)).align(Align::Center), text.color(palette.text_secondary));
        }
        cv.pop_clip();
        self.paint_scrollbar(cv, rect, dpi, palette);
    }

    /// Grid and list layouts: an icon with its name, packed into columns.
    /// There is no column header here — the icon size *is* the view mode.
    fn paint_cells(&self, cv: &Canvas, rect: RectF, dpi: u32, palette: &Palette, icons: &[Option<ID2D1Bitmap>]) {
        let s = |value: f32| value * dpi as f32 / 96.0;
        let text = TextField { palette, dpi };
        let first = self.visible_rows(rect, dpi).start;
        cv.push_clip(rect);
        for index in self.visible_rows(rect, dpi) {
            let cell = self.cell_rect(rect, dpi, index);
            if cell.bottom() <= rect.y || cell.y >= rect.bottom() { continue; }
            let selected = self.selection.contains(&index);
            let hovered = self.hover == Some(index);
            let targeted = self.drop_target == Some(index);
            let chip = RectF::new(cell.x + s(3.0), cell.y + s(3.0), (cell.w - s(6.0)).max(0.0), (cell.h - s(6.0)).max(1.0));
            if targeted {
                cv.round(chip, s(5.0), zsdesk_windows::gfx::Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0x2e));
                cv.stroke_round(chip, s(5.0), zsdesk_windows::gfx::Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0xB0), s(1.0));
            } else if selected {
                cv.round(chip, s(5.0), text.accent(palette.accent));
            } else if hovered {
                cv.round(chip, s(5.0), text.color(palette.hover));
            }
            let color = text.color(palette.text);
            let Some(entry) = self.items.get(index) else { continue };
            let (icon_rect, lines) = self.cell_text(cell, dpi);
            match icons.get(index - first) {
                Some(Some(bitmap)) => cv.image(bitmap, fitted(icon_rect, bitmap), 1.0),
                _ => {
                    let glyph = if entry.is_dir { zsdesk_ui::theme::glyph::FOLDER } else { FILE_GLYPH };
                    cv.glyph(glyph, icon_rect, s(icon_rect.h * 0.75), color);
                }
            }
            let Some(name_rect) = lines.first() else { continue };
            let align = if matches!(self.layout, Layout::Icons { .. }) { Align::Center } else { Align::Start };
            // The editor draws the name while it is open; painting the old one
            // underneath would show two names once the user starts typing.
            if self.editing != Some(index) {
                cv.text(&entry.name, *name_rect, &TextStyle::ui(s(12.5)).align(align), color);
            }
            if let Some(detail) = lines.get(1) {
                // 平铺 / 内容 show a second line, like Explorer's tiles.
                let mut summary = entry.kind();
                let size = size_text(entry.size, entry.is_dir);
                if !size.is_empty() { summary.push_str("   "); summary.push_str(&size); }
                cv.text(&summary, *detail, &TextStyle::ui(s(11.5)).align(align), text.color(palette.text_secondary));
            }
        }
        if self.items.is_empty() {
            let message = self.empty_text("此文件夹为空");
            cv.text(&message, rect, &TextStyle::ui(s(12.5)).align(Align::Center), text.color(palette.text_secondary));
        }
        cv.pop_clip();
        self.paint_scrollbar(cv, rect, dpi, palette);
    }

    /// Windows 11 style: a rounded thumb on the right edge with no chrome. The
    /// lane only appears under the pointer, so the pane stays clean otherwise.
    fn paint_scrollbar(&self, cv: &Canvas, rect: RectF, dpi: u32, palette: &Palette) {
        let s = |value: f32| value * dpi as f32 / 96.0;
        let Some((track, thumb)) = self.scrollbar(rect, dpi) else { return };
        let active = self.bar_drag.is_some();
        let hot = self.bar_hot || active;
        let dark = palette.mode == zsdesk_ui::theme::Mode::Dark;
        let lane = if dark { (0xFF, 0xFF, 0xFF) } else { (0x00, 0x00, 0x00) };
        if hot {
            cv.round(track, s(BAR_W) / 2.0, zsdesk_windows::gfx::Color::rgba8(lane.0, lane.1, lane.2, if active { 0x1f } else { 0x14 }));
        }
        let alpha = match (dark, hot) { (true, true) => 0x66, (true, false) => 0x33, (false, true) => 0x4d, (false, false) => 0x26 };
        cv.round(thumb, thumb.w / 2.0, zsdesk_windows::gfx::Color::rgba8(lane.0, lane.1, lane.2, alpha));
    }
}

struct TextField<'a> { palette: &'a Palette, dpi: u32 }
impl TextField<'_> {
    fn color(&self, color: zsdesk_ui::theme::Color) -> zsdesk_windows::gfx::Color {
        zsdesk_windows::gfx::Color::rgba8(color.r, color.g, color.b, color.a)
    }
    fn accent(&self, color: zsdesk_ui::theme::Color) -> zsdesk_windows::gfx::Color {
        zsdesk_windows::gfx::Color::rgba8(color.r, color.g, color.b, 0x54)
    }
}

fn cell(rect: RectF) -> RectF { RectF::new(rect.x.round(), rect.y.round(), rect.w.round().max(1.0), rect.h.round().max(1.0)) }

/// Fits an image inside the icon cell without stretching it.
///
/// The Shell returns a thumbnail at its own aspect ratio (a 16:9 video comes
/// back as 16×9), so drawing it into a square cell would squash it; icons are
/// already square and keep the whole cell.
fn fitted(rect: RectF, bitmap: &ID2D1Bitmap) -> RectF {
    let size = unsafe { bitmap.GetSize() };
    if size.width <= 0.0 || size.height <= 0.0 { return cell(rect); }
    let scale = (rect.w / size.width).min(rect.h / size.height);
    let (w, h) = (size.width * scale, size.height * scale);
    RectF::new(rect.x + (rect.w - w) / 2.0, rect.y + (rect.h - h) / 2.0, w.max(1.0), h.max(1.0))
}

/// X where a details row starts its name: after the icon and its gap.
///
/// The painter and the rename editor both go through this, so the editor can
/// never drift from the letters it is supposed to cover.
fn name_left(row: RectF, s: impl Fn(f32) -> f32) -> f32 {
    row.x + s(PAD + ICON + 6.0)
}

/// The Shell reports display names, which drop a known extension or carry a
/// folder's `desktop.ini` rename while the row keeps the real file name.
fn same_name(entry: &str, shell: &str) -> bool {
    if entry.eq_ignore_ascii_case(shell) { return true; }
    std::path::Path::new(entry).file_stem()
        .is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case(shell))
}

fn sort_items(items: &mut [FolderEntry], key: SortKey, ascending: bool) {
    // 此电脑 holds drives, not files: letter order is the only order that makes
    // sense there, and Explorer keeps it whatever the column sort says.
    if items.iter().any(|entry| entry.drive.is_some()) {
        items.sort_by(|a, b| match (&a.drive, &b.drive) {
            (Some(a), Some(b)) => a.letter.cmp(&b.letter),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => zsdesk_core::model::view::natural_cmp(&a.name, &b.name),
        });
        return;
    }
    items.sort_by(|a, b| {
        // Folders first in both directions, like Explorer: reversing the
        // sort must not hide them at the bottom of the list.
        let folder = b.is_dir.cmp(&a.is_dir);
        if folder != std::cmp::Ordering::Equal { return folder; }
        let order = match key {
            SortKey::Name => zsdesk_core::model::view::natural_cmp(&a.name, &b.name),
            SortKey::Kind => zsdesk_core::model::view::natural_cmp(&a.kind(), &b.kind())
                .then_with(|| zsdesk_core::model::view::natural_cmp(&a.name, &b.name)),
            SortKey::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0))
                .then_with(|| zsdesk_core::model::view::natural_cmp(&a.name, &b.name)),
            SortKey::Modified => a.modified.unwrap_or(0).cmp(&b.modified.unwrap_or(0))
                .then_with(|| zsdesk_core::model::view::natural_cmp(&a.name, &b.name)),
            SortKey::Created => a.created.unwrap_or(0).cmp(&b.created.unwrap_or(0))
                .then_with(|| zsdesk_core::model::view::natural_cmp(&a.name, &b.name)),
        };
        if ascending { order } else { order.reverse() }
    });
}

fn modified_text(modified: Option<u64>) -> String {
    let Some(ms) = modified else { return String::new() };
    let seconds = (ms / 1000) as i64;
    let (days, remainder) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    let (hour, minute) = (remainder / 3600, (remainder % 3600) / 60);
    format!("{year:04}/{month:02}/{day:02} {hour:02}:{minute:02}")
}

/// Howard Hinnant's civil-from-days, so no time crate is needed.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

pub(super) fn size_text(size: Option<u64>, directory: bool) -> String {
    if directory { return String::new(); }
    let Some(bytes) = size else { return String::new() };
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() { value /= 1024.0; unit += 1; }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, directory: bool, size: Option<u64>, modified: Option<u64>) -> FolderEntry {
        FolderEntry { name: name.into(), parsing_name: name.into(), path: None, is_dir: directory,
            hidden: false, size, modified, created: None, icon: 0, drive: None }
    }

    /// A "在文件夹中显示" request names a file; the list must highlight that row
    /// (by label or real name) and say so, or the caller keeps retrying and the
    /// old selection stays on screen looking like the answer.
    #[test]
    fn a_reveal_selects_the_named_row_or_reports_a_miss() {
        let mut view = FileView::default();
        view.items = vec![entry("alpha.txt", false, None, None), entry("报告.docx", false, None, None),
            entry("zulu.txt", false, None, None)];
        assert!(view.select_names(&["zulu.txt".into()]));
        assert_eq!(view.file_names(), ["zulu.txt".to_string()]);
        assert!(view.select_names(&["报告.docx".into(), "alpha.txt".into()]));
        assert_eq!(view.selection.len(), 2);
        // Not in the folder yet (still enumerating): the request must not stick
        // a wrong row into the selection.
        assert!(!view.select_names(&["missing.txt".into()]));
        assert_eq!(view.selection.len(), 2, "a failed reveal leaves the selection alone");
    }

    #[test]
    fn sorting_keeps_folders_first_and_follows_natural_order() {        let mut items = vec![entry("b.txt", false, Some(2), None), entry("a", true, None, None), entry("a10.txt", false, Some(3), None), entry("a2.txt", false, Some(1), None)];
        sort_items(&mut items, SortKey::Name, true);
        let names: Vec<_> = items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["a", "a2.txt", "a10.txt", "b.txt"]);
        sort_items(&mut items, SortKey::Size, true);
        let names: Vec<_> = items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["a", "a2.txt", "b.txt", "a10.txt"]);
    }

    #[test]
    fn shift_click_selects_a_range_from_the_anchor() {
        let mut view = FileView::default();
        view.items = (0..6).map(|index| entry(&format!("f{index}"), false, None, None)).collect();
        view.loaded = true;
        assert_eq!(view.click(Some(1), false, false), ["f1"]);
        let range = view.click(Some(4), false, true);
        assert_eq!(range, ["f1", "f2", "f3", "f4"]);
        let range = view.click(Some(0), false, true);
        assert_eq!(range, ["f0", "f1"]);
        assert!(view.click(None, false, false).is_empty());
    }

    #[test]
    fn control_click_toggles_one_row_without_touching_the_rest() {
        let mut view = FileView::default();
        view.items = (0..3).map(|index| entry(&format!("f{index}"), false, None, None)).collect();
        view.click(Some(0), false, false);
        assert_eq!(view.click(Some(2), true, false), ["f0", "f2"]);
        assert_eq!(view.click(Some(0), true, false), ["f2"]);
    }

    #[test]
    fn sizes_and_dates_are_human_readable() {
        assert_eq!(size_text(Some(512), false), "512 B");
        assert_eq!(size_text(Some(1536), false), "1.5 KB");
        assert_eq!(size_text(None, true), "");
        assert_eq!(modified_text(Some(0)), "1970/01/01 00:00");
    }

    #[test]
    fn hit_testing_maps_rows_and_the_header() {
        let view = FileView { items: (0..3).map(|index| entry(&format!("f{index}"), false, None, None)).collect(), loaded: true, ..Default::default() };
        let rect = RectF::new(0.0, 0.0, 400.0, 300.0);
        assert_eq!(view.hit(rect, 96, 20, 10), ViewHit::Header(Column::Name));
        assert_eq!(view.hit(rect, 96, 20, 30), ViewHit::Row(0));
        assert_eq!(view.hit(rect, 96, 20, 30 + 24 * 2), ViewHit::Row(2));
        assert_eq!(view.hit(rect, 96, 20, 250), ViewHit::Empty);
        assert_eq!(view.hit(rect, 96, 500, 30), ViewHit::None);
    }

    #[test]
    fn a_folder_that_fits_has_no_scrollbar_and_keeps_the_full_width() {
        let view = FileView { items: (0..3).map(|index| entry(&format!("f{index}"), false, None, None)).collect(), loaded: true, ..Default::default() };
        let rect = RectF::new(0.0, 0.0, 400.0, 300.0);
        assert!(view.scrollbar(rect, 96).is_none());
        assert_eq!(view.content_rect(rect, 96), rect);
        // The right edge still belongs to the last column.
        assert_eq!(view.hit(rect, 96, 398, 30), ViewHit::Row(0));
    }

    #[test]
    fn a_long_folder_scrolls_by_dragging_the_thumb() {
        let mut view = FileView { items: (0..100).map(|index| entry(&format!("f{index}"), false, None, None)).collect(), loaded: true, ..Default::default() };
        let rect = RectF::new(0.0, 0.0, 400.0, 300.0);
        view.set_viewport(rect, 96);
        let (track, thumb) = view.scrollbar(rect, 96).expect("100 rows do not fit in 300 px");
        assert_eq!(track.right(), rect.right());
        assert!(thumb.h >= 28.0 && thumb.h < track.h);
        // The bar keeps its own lane: a cell never runs under the thumb.
        assert!(view.content_rect(rect, 96).w < rect.w);
        assert_eq!(view.hit(rect, 96, (thumb.x + 2.0) as i32, (thumb.y + 4.0) as i32), ViewHit::ScrollBar { thumb: true });
        assert_eq!(view.hit(rect, 96, (track.x + 2.0) as i32, (thumb.bottom() + 40.0) as i32), ViewHit::ScrollBar { thumb: false });
        // The column header still spans the whole pane, lane included.
        assert_eq!(view.hit(rect, 96, (track.x + 2.0) as i32, 10), ViewHit::Header(Column::Size));

        // Grab the thumb near its top and drag it to the bottom of the lane.
        assert!(view.press_bar(rect, 96, thumb.y + 4.0, true));
        let max = view.max_scroll(96);
        view.drag_bar(rect, 96, track.bottom() + 4.0);
        assert!((view.scroll - max).abs() < 0.5, "dragging to the end reaches the last page");

        // And back to the top.
        view.drag_bar(rect, 96, track.y - 40.0);
        assert_eq!(view.scroll, 0.0);
        assert!(view.end_bar_drag());
        assert!(!view.dragging_bar());
    }

    #[test]
    fn clicking_the_lane_pages_instead_of_dragging() {
        let mut view = FileView { items: (0..100).map(|index| entry(&format!("f{index}"), false, None, None)).collect(), loaded: true, ..Default::default() };
        let rect = RectF::new(0.0, 0.0, 400.0, 300.0);
        view.set_viewport(rect, 96);
        let (track, thumb) = view.scrollbar(rect, 96).expect("a long folder");
        // Below the thumb: one page down, no drag started.
        assert!(!view.press_bar(rect, 96, thumb.bottom() + 60.0, false));
        assert!(!view.dragging_bar());
        assert_eq!(view.scroll, track.h - view.row_height(96));
        // Above it: one page back up.
        view.press_bar(rect, 96, thumb.y - 30.0, false);
        assert_eq!(view.scroll, 0.0);
    }

    #[test]
    fn a_listing_that_arrives_after_another_navigation_is_dropped() {
        let mut view = FileView::default();
        let stale = view.begin_load(&ShellLocation::Path("C:\\old".into()));
        let current = view.begin_load(&ShellLocation::Path("C:\\new".into()));
        // The folder changed: the old rows must not show under the new title.
        assert!(!view.loaded && view.items.is_empty());
        assert!(!view.finish_load(stale, Ok(vec![entry("old.txt", false, None, None)])), "a stale listing never lands");
        assert!(view.items.is_empty());
        assert!(view.finish_load(current, Ok(vec![entry("new.txt", false, None, None)])));
        assert!(view.loaded);
        assert_eq!(view.items.len(), 1);
    }

    #[test]
    fn relisting_the_same_folder_keeps_the_rows_and_the_selection_until_it_lands() {
        let location = ShellLocation::Path("C:\\work".into());
        let mut view = FileView::default();
        let token = view.begin_load(&location);
        view.finish_load(token, Ok(vec![entry("a.txt", false, Some(4), None), entry("b.txt", false, Some(6), None)]));
        view.click(Some(1), false, false);
        assert_eq!(view.selected_bytes(), Some(6));

        let refresh = view.begin_load(&location);
        assert!(view.loaded, "a refresh keeps the rows on screen");
        assert_eq!(view.selected_bytes(), Some(6));
        assert!(view.finish_load(refresh, Ok(vec![entry("a.txt", false, Some(4), None), entry("b.txt", false, Some(6), None), entry("c.txt", false, Some(2), None)])));
        assert_eq!(view.names(), ["b.txt"], "the refresh restores the same selection");
    }

    #[test]
    fn an_unreadable_folder_reports_the_error_instead_of_a_listing() {
        let mut view = FileView::default();
        let token = view.begin_load(&ShellLocation::Path("Z:\\missing".into()));
        assert!(view.finish_load(token, Err("拒绝访问".into())));
        assert!(!view.loaded);
        assert!(view.items.is_empty());
        assert!(view.error.as_deref().is_some_and(|error| error.contains("拒绝访问")));
    }

    #[test]
    fn every_view_mode_has_a_layout_and_a_tick_of_its_own() {
        for (_, id, _) in zsdesk_ui::menus::VIEW_CHOICES {
            let layout = Layout::from_id(id).unwrap_or_else(|| panic!("no layout for {id}"));
            assert_eq!(layout.id(), *id, "the menu must tick the mode it just applied");
        }
        assert_eq!(Layout::from_id("details"), Some(Layout::Details));
        assert_eq!(Layout::default(), Layout::Details);
    }

    #[test]
    fn a_grid_cell_centre_hits_the_row_that_was_drawn() {
        let mut view = FileView::default();
        view.items = (0..7).map(|index| entry(&format!("f{index}"), false, Some(index as u64), None)).collect();
        view.loaded = true;
        for layout in [
            Layout::Listing,
            Layout::Content,
            Layout::Icons { icon: 96.0, lines: 1 },
            Layout::Icons { icon: 48.0, lines: 2 },
            Layout::Icons { icon: 24.0, lines: 1 },
        ] {
            view.set_layout(layout);
            let rect = RectF::new(0.0, 0.0, 640.0, 400.0);
            view.set_viewport(rect, 96);
            for index in 0..view.items.len() {
                let cell = view.row_rect(rect, 96, index);
                let hit = view.hit(rect, 96, (cell.x + cell.w / 2.0) as i32, (cell.y + cell.h / 2.0) as i32);
                assert_eq!(hit, ViewHit::Row(index), "{layout:?} cell {index} at {cell:?}");
            }
        }
    }

    #[test]
    fn switching_the_layout_keeps_the_selection_and_returns_to_the_top() {
        let mut view = FileView::default();
        view.items = (0..30).map(|index| entry(&format!("f{index}"), false, None, None)).collect();
        view.loaded = true;
        let rect = RectF::new(0.0, 0.0, 400.0, 200.0);
        view.set_viewport(rect, 96);
        view.click(Some(5), false, false);
        view.scroll_by(300.0, 96);
        assert!(view.set_layout(Layout::Icons { icon: 96.0, lines: 1 }));
        assert_eq!(view.scroll, 0.0);
        assert_eq!(view.names(), ["f5"]);
        assert!(!view.set_layout(Layout::Icons { icon: 96.0, lines: 1 }), "the same layout is a no-op");
    }

    #[test]
    fn sorting_by_created_time_and_reversing_keeps_folders_first() {
        let with_created = |name: &str, directory: bool, created: u64| {
            FolderEntry { name: name.into(), parsing_name: name.into(), path: None, is_dir: directory,
                hidden: false, size: None, modified: None, created: Some(created), icon: 0, drive: None }
        };
        let mut items = vec![with_created("b.txt", false, 20), with_created("dir", true, 99), with_created("a.txt", false, 10)];
        sort_items(&mut items, SortKey::Created, true);
        assert_eq!(items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["dir", "a.txt", "b.txt"]);
        sort_items(&mut items, SortKey::Created, false);
        assert_eq!(items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["dir", "b.txt", "a.txt"],
            "descending flips the files, never the folder-first rule");
    }

    #[test]
    fn sort_by_forces_the_direction_the_menu_asked_for() {
        let mut view = FileView::default();
        view.items = (0..3).map(|index| entry(&format!("f{index}"), false, Some(index as u64), None)).collect();
        view.loaded = true;
        view.sort_by(SortKey::Size, true);
        assert_eq!(view.items.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["f0", "f1", "f2"]);
        view.sort_by(SortKey::Size, false);
        assert_eq!(view.items.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["f2", "f1", "f0"]);
        assert_eq!(view.sort_order(), Some((SortKey::Size, false)));
        view.set_sort(SortKey::Size);
        assert_eq!(view.sort_order(), Some((SortKey::Size, true)), "picking the same key flips it");
    }

    #[test]
    fn this_pc_draws_device_cards_that_hit_where_they_are_painted() {
        let drive = |letter: char, total: u64| FolderEntry {
            name: format!("磁盘 ({letter}:)"), parsing_name: format!("{letter}:\\"),
            path: Some(std::path::PathBuf::from(format!("{letter}:\\"))), is_dir: true, hidden: false,
            size: None, modified: None, created: None, icon: 0,
            drive: Some(drives::Drive { letter, kind: drives::DriveKind::Fixed, total, free: total / 2 }),
        };
        let mut view = FileView::default();
        let token = view.begin_load(&ShellLocation::ThisPc);
        // The listing arrives in whatever order; the view sorts by letter.
        assert!(view.finish_load(token, Ok(vec![drive('E', 300), drive('C', 100), drive('D', 200)])));
        let rect = RectF::new(0.0, 0.0, 800.0, 600.0);
        view.set_viewport(rect, 96);
        assert!(view.is_devices());
        assert_eq!(view.items.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            ["磁盘 (C:)", "磁盘 (D:)", "磁盘 (E:)"]);
        for index in 0..3 {
            let cell = view.row_rect(rect, 96, index);
            assert_eq!(view.hit(rect, 96, (cell.x + cell.w / 2.0) as i32, (cell.y + cell.h / 2.0) as i32),
                ViewHit::Row(index), "card {index} at {cell:?}");
        }
        // The group title and the margin are not cards.
        assert_eq!(view.hit(rect, 96, 40, 10), ViewHit::Empty);
        assert_eq!(view.hit(rect, 96, 4, 100), ViewHit::Empty);
        assert_eq!(view.icon_px(96), 32, "a card asks for a 32 px volume icon");

        // A sort key never reorders drives: letter order is the only one that
        // matches what Explorer shows under 此电脑.
        view.sort_by(SortKey::Size, false);
        assert_eq!(view.items.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            ["磁盘 (C:)", "磁盘 (D:)", "磁盘 (E:)"]);
    }

    #[test]
    fn dragging_a_boundary_resizes_the_column_on_its_right() {
        let mut view = FileView { items: (0..5).map(|index| entry(&format!("f{index}"), false, None, None)).collect(), loaded: true, ..Default::default() };
        let rect = RectF::new(0.0, 0.0, 800.0, 300.0);
        let columns = view.columns(rect, 96);
        // The 名称 | 修改日期 boundary sits at the start of the second column.
        let divider = columns[1].1.x;
        assert_eq!(view.hit(rect, 96, divider as i32, 5), ViewHit::HeaderDivider(0), "a press on the boundary sizes instead of sorting");
        assert_eq!(view.hit(rect, 96, divider as i32 + 20, 5), ViewHit::Header(Column::Modified));
        assert_eq!(view.hit(rect, 96, divider as i32, 40), ViewHit::Row(0), "rows keep their click target");

        assert!(view.press_divider(rect, 96, divider));
        assert!(view.dragging_divider());
        view.drag_divider(rect, 96, divider + 40.0);
        assert_eq!(view.column_widths_dip(), [92, 96, 86], "the column on the right gave up 40 px");
        assert!(view.end_divider_drag());
        assert!(!view.dragging_divider());

        // The boundary followed the pointer: 名称 grew by the same 40 px.
        let after = view.columns(rect, 96);
        assert!((after[0].1.w - (columns[0].1.w + 40.0)).abs() < 0.5);
        assert!((after[1].1.x - (divider + 40.0)).abs() < 0.5);
    }

    #[test]
    fn a_boundary_never_collapses_a_column_or_squeezes_the_names_out() {
        let mut view = FileView::default();
        let rect = RectF::new(0.0, 0.0, 800.0, 300.0);
        let divider = view.columns(rect, 96)[1].1.x;
        // Dragging right stops at the column's minimum width.
        assert!(view.press_divider(rect, 96, divider));
        view.drag_divider(rect, 96, divider + 5000.0);
        assert_eq!(view.column_widths_dip()[0], MIN_TAIL[0] as i32);
        // Dragging left stops where the name column hits its own minimum.
        view.drag_divider(rect, 96, divider - 5000.0);
        let columns = view.columns(rect, 96);
        assert!(columns[0].1.w >= MIN_NAME - 0.5, "the name column kept its room: {:?}", columns[0].1);
        assert!(columns.last().unwrap().1.right() <= rect.right() + 0.5, "the table never leaves the pane");
        assert!(view.end_divider_drag());
    }

    #[test]
    fn column_widths_are_stored_in_dip_so_a_dpi_change_keeps_the_table() {
        let mut view = FileView::default();
        view.set_column_widths([100, 80, 70]);
        assert_eq!(view.column_widths_dip(), [100, 80, 70]);
        let normal = view.columns(RectF::new(0.0, 0.0, 1200.0, 300.0), 96);
        let hidpi = view.columns(RectF::new(0.0, 0.0, 1800.0, 450.0), 144);
        for index in 1..4 {
            assert!((hidpi[index].1.w - normal[index].1.w * 1.5).abs() < 0.5, "column {index} scales with the DPI");
        }
        // A session that stored nothing, or a broken value, falls back to the
        // defaults instead of collapsing a column to zero.
        let mut fresh = FileView::default();
        fresh.set_column_widths([0, 140, -7]);
        assert_eq!(fresh.column_widths_dip(), [DEFAULT_TAIL[0] as i32, 140, DEFAULT_TAIL[2] as i32]);
    }

    #[test]
    fn a_narrow_pane_squeezes_the_tail_instead_of_cutting_it_off() {
        let view = FileView::default();
        let narrow = RectF::new(0.0, 0.0, 300.0, 200.0);
        let columns = view.columns(narrow, 96);
        assert!(columns[0].1.w >= MIN_NAME - 0.5, "the names keep their room: {:?}", columns[0].1);
        assert!((columns.last().unwrap().1.right() - narrow.right()).abs() <= 0.5, "the last column still ends at the pane edge");
        assert!(columns.iter().all(|(_, area)| area.w > 0.0));
    }

    #[test]
    fn a_new_search_page_replaces_rows_even_for_the_same_query() {
        let search = ShellLocation::Search { query: "alpha".into(), scope: None };
        let mut view = FileView::default();
        let first = view.restart(&search);
        assert!(view.append_load(first, Ok(vec![entry("a.txt", false, Some(1), None)])));
        // Page two asks the same question: `begin_load` would keep the rows,
        // `restart` must not, or the new page would grow the old one.
        let second = view.restart(&search);
        assert!(view.items.is_empty(), "a new page starts from an empty table");
        assert!(view.append_load(second, Ok(vec![entry("b.txt", false, Some(1), None)])));
        assert_eq!(view.items.len(), 1);
        // Rows that come back after the next page started belong to the old one.
        assert!(!view.append_load(first, Ok(vec![entry("stale.txt", false, None, None)])));
        assert_eq!(view.items.len(), 1);
    }

    #[test]
    fn a_selection_matches_the_name_the_shell_reports() {
        // The Shell drops a known extension and a folder's desktop.ini rename.
        assert!(same_name("报告.txt", "报告"));
        assert!(same_name("桌面", "桌面"));
        assert!(same_name("Report.TXT", "report.txt"));
        assert!(!same_name("报告.txt", "报告.doc"));
        assert!(!same_name("a", "b"));
    }

    /// The rename editor is placed on `name_rect`, so that rectangle has to be
    /// the one the painter writes the name into — not a guessed offset.
    #[test]
    fn the_rename_editor_lands_on_the_painted_name() {
        let mut view = FileView { items: vec![entry("报告.docx", false, None, None)], loaded: true, ..Default::default() };
        let rect = RectF::new(0.0, 0.0, 800.0, 400.0);
        view.set_viewport(rect, 96);
        let row = view.row_rect(rect, 96, 0);
        let area = view.name_rect(rect, 96, 0).expect("a details row paints a name");
        // After the 16 px icon and its gap, using the row's own height so the
        // text keeps the same centre line.
        assert_eq!(area.x, row.x + PAD + ICON + 6.0);
        assert_eq!((area.y, area.h), (row.y, row.h));
        let content = view.content_rect(rect, 96);
        let name_column = view.columns(RectF::new(content.x, row.y, content.w, row.h), 96)[0].1;
        assert_eq!(area.right(), name_column.right() - 6.0, "the field stops where the name column does");
    }

    #[test]
    fn an_icon_layout_centres_the_rename_editor_on_its_label() {
        let mut view = FileView { items: vec![entry("图片.png", false, None, None)], loaded: true, ..Default::default() };
        let rect = RectF::new(0.0, 0.0, 800.0, 400.0);
        for layout in [Layout::Icons { icon: 96.0, lines: 1 }, Layout::Icons { icon: 48.0, lines: 2 }, Layout::Content, Layout::Listing] {
            view.set_layout(layout);
            view.set_viewport(rect, 96);
            let cell = view.row_rect(rect, 96, 0);
            let area = view.name_rect(rect, 96, 0).expect("every layout paints a name");
            let (icon, lines) = view.cell_text(cell, 96);
            assert_eq!(Some(area), lines.first().copied(), "{layout:?} must rename on the label band");
            if matches!(layout, Layout::Icons { .. }) {
                // Icons put the label under the icon, and the label is centred,
                // so the editor has to centre its text too.
                assert!(area.y >= icon.bottom(), "{layout:?} label sits under the icon");
                assert!(view.centers_names());
            } else {
                assert!(area.x > icon.x, "{layout:?} puts the label beside the icon");
            }
        }
    }

    #[test]
    fn an_edited_row_drops_its_painted_name_so_only_the_editor_shows() {
        let mut view = FileView { items: vec![entry("a.txt", false, None, None), entry("b.txt", false, None, None)], loaded: true, ..Default::default() };
        assert_eq!(view.editing, None);
        view.set_editing(Some(1));
        assert_eq!(view.editing, Some(1), "the row under the editor is the one that stops painting its name");
        view.set_editing(None);
        assert_eq!(view.editing, None);
    }
}
