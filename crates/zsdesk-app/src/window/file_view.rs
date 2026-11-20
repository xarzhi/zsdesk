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
    gfx::{Align, Canvas, RectF, TextStyle},
    shell::folder_items::FolderEntry,
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
/// Segoe Fluent "Page" glyph used for files without an icon.
const FILE_GLYPH: &str = "\u{E7C3}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Column { Name, Modified, Kind, Size }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SortKey { Name, Modified, Kind, Size }

impl Column {
    pub(super) fn key(self) -> SortKey {
        match self { Column::Name => SortKey::Name, Column::Modified => SortKey::Modified, Column::Kind => SortKey::Kind, Column::Size => SortKey::Size }
    }
    pub(super) fn title(self) -> &'static str {
        match self { Column::Name => "名称", Column::Modified => "修改日期", Column::Kind => "类型", Column::Size => "大小" }
    }
    fn width(self, dpi: u32) -> f32 {
        let s = |value: f32| value * dpi as f32 / 96.0;
        match self { Column::Name => 0.0, Column::Modified => s(132.0), Column::Kind => s(96.0), Column::Size => s(86.0) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ViewHit { None, Header(Column), Row(usize), Empty, ScrollBar { thumb: bool } }

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
    pub location: Option<ShellLocation>,
    /// False until the folder produced a listing (or failed to).
    pub loaded: bool,
    pub error: Option<String>,
    /// Bumped by every request; a listing that comes back with an older token
    /// belongs to a folder the user has already left.
    token: u64,
    /// Pointer offset inside the thumb while it is being dragged.
    bar_drag: Option<f32>,
    /// Pointer is over the scrollbar lane, so it is drawn stronger.
    bar_hot: bool,
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

    pub(super) fn set_sort(&mut self, key: SortKey) {
        if self.sort == Some(key) { self.ascending = !self.ascending; } else { self.sort = Some(key); self.ascending = true; }
        sort_items(&mut self.items, key, self.ascending);
        self.selection.clear();
        self.anchor = None;
        self.scroll = 0.0;
    }

    pub(super) fn sort_state(&self, key: SortKey) -> Option<bool> {
        (self.sort == Some(key)).then_some(self.ascending)
    }

    fn row_height(&self, dpi: u32) -> f32 { ROW_H * dpi as f32 / 96.0 }
    fn header_height(&self, dpi: u32) -> f32 { HEADER_H * dpi as f32 / 96.0 }

    /// Column rectangles for the current viewport; the name column takes the rest.
    pub(super) fn columns(&self, rect: RectF, dpi: u32) -> Vec<(Column, RectF)> {
        let tail = [Column::Modified, Column::Kind, Column::Size];
        let mut used = 0.0;
        let mut widths = Vec::new();
        for column in tail { let width = column.width(dpi).min(rect.w * 0.25); widths.push(width); used += width; }
        let name = (rect.w - used).max(rect.w * 0.3);
        let mut result = vec![(Column::Name, RectF::new(rect.x, rect.y, name, rect.h))];
        let mut x = rect.x + name;
        for (column, width) in tail.into_iter().zip(widths) {
            result.push((column, RectF::new(x, rect.y, width, rect.h)));
            x += width;
        }
        result
    }

    pub(super) fn visible_rows(&self, rect: RectF, dpi: u32) -> std::ops::Range<usize> {
        let row = self.row_height(dpi);
        if row <= 0.0 { return 0..0; }
        let header = self.header_height(dpi);
        let body = (rect.h - header).max(0.0);
        let first = (self.scroll / row).floor().max(0.0) as usize;
        let count = (body / row).ceil() as usize + 1;
        first.min(self.items.len())..(first + count).min(self.items.len())
    }

    pub(super) fn hit(&self, rect: RectF, dpi: u32, x: i32, y: i32) -> ViewHit {
        if !rect.contains(x as f32, y as f32) { return ViewHit::None; }
        if let Some((track, thumb)) = self.scrollbar(rect, dpi) {
            if (x as f32) >= track.x && (y as f32) >= track.y {
                return ViewHit::ScrollBar { thumb: thumb.contains(x as f32, y as f32) };
            }
        }
        let rect = self.content_rect(rect, dpi);
        let header = self.header_height(dpi);
        let row = self.row_height(dpi);
        if (y as f32) < rect.y + header {
            for (column, area) in self.columns(RectF::new(rect.x, rect.y, rect.w, header), dpi) {
                if (x as f32) >= area.x && (x as f32) < area.right() { return ViewHit::Header(column); }
            }
            return ViewHit::Header(Column::Name);
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

    /// Real file names of the selection: what the Shell parses without guessing
    /// at a localized or extension-less display name.
    pub(super) fn file_names(&self) -> Vec<String> {
        self.selection.iter().filter_map(|index| self.items.get(*index)).map(FolderEntry::file_name).collect()
    }

    /// Total size of the selected files, for the status bar.
    pub(super) fn selected_bytes(&self) -> Option<u64> {
        if self.selection.is_empty() { return None; }
        let mut total = 0u64;
        for index in &self.selection {
            total = total.checked_add(self.items.get(*index).and_then(|entry| entry.size).unwrap_or(0))?;
        }
        Some(total)
    }

    /// Replaces the selection from names reported by the Shell view.
    pub(super) fn adopt(&mut self, names: &[String]) {
        self.selection.clear();
        if names.is_empty() { return; }
        for (index, entry) in self.items.iter().enumerate() {
            if names.iter().any(|name| same_name(&entry.name, name)) { self.selection.insert(index); }
        }
    }

    pub(super) fn set_hover(&mut self, hover: Option<usize>) -> bool {
        if self.hover == hover { return false; }
        self.hover = hover;
        true
    }

    /// Moves the selection by `delta` rows (arrows, page keys, home/end).
    pub(super) fn step(&mut self, delta: isize, extend: bool) {
        if self.items.is_empty() { return; }
        let last = self.items.len() - 1;
        let current = self.selection.iter().next_back().copied().unwrap_or(0);
        let next = (current as isize + delta).clamp(0, last as isize) as usize;
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
        self.clamp_scroll(dpi);
    }

    pub(super) fn scroll_by(&mut self, delta: f32, dpi: u32) {
        self.scroll += delta;
        self.clamp_scroll(dpi);
    }

    /// Scrolls the focused row into view.
    pub(super) fn reveal_focus(&mut self, dpi: u32) {
        let Some(index) = self.focus() else { return };
        let row = self.row_height(dpi);
        let top = index as f32 * row;
        let bottom = top + row;
        let viewport = (self.height as f32 - self.header_height(dpi)).max(row);
        if top < self.scroll { self.scroll = top; }
        else if bottom > self.scroll + viewport { self.scroll = bottom - viewport; }
        self.clamp_scroll(dpi);
    }

    fn clamp_scroll(&mut self, dpi: u32) {
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(dpi));
    }

    /// Height of the rows below the column header.
    fn body_height(&self, dpi: u32) -> f32 {
        (self.height as f32 - self.header_height(dpi)).max(0.0)
    }

    /// How far the list can scroll; zero while every row fits.
    pub(super) fn max_scroll(&self, dpi: u32) -> f32 {
        self.max_scroll_in(self.height as f32, dpi)
    }

    /// Same, for a pane height the caller is drawing into right now.
    fn max_scroll_in(&self, height: f32, dpi: u32) -> f32 {
        (self.items.len() as f32 * self.row_height(dpi) - (height - self.header_height(dpi)).max(0.0)).max(0.0)
    }

    /// Scrollbar lane and thumb, or `None` when the rows all fit. The lane
    /// starts under the column header so the header keeps its full width.
    pub(super) fn scrollbar(&self, rect: RectF, dpi: u32) -> Option<(RectF, RectF)> {
        let s = |value: f32| value * dpi as f32 / 96.0;
        let max = self.max_scroll_in(rect.h, dpi);
        let header = self.header_height(dpi);
        if max <= 0.0 || rect.h <= header { return None; }
        let track = RectF::new(rect.right() - s(BAR_W), rect.y + header, s(BAR_W), rect.h - header);
        let content = self.items.len() as f32 * self.row_height(dpi);
        let thumb_h = (track.h * (track.h / content.max(1.0))).clamp(s(BAR_MIN_THUMB), track.h);
        let travel = (track.h - thumb_h).max(0.0);
        let y = track.y + (self.scroll / max) * travel;
        let thumb = RectF::new(track.x + (s(BAR_W) - s(BAR_THUMB_W)) / 2.0, y, s(BAR_THUMB_W), thumb_h);
        Some((track, thumb))
    }

    /// The pane without the scrollbar lane: where rows and columns live, so a
    /// long name never runs under the thumb.
    pub(super) fn content_rect(&self, rect: RectF, dpi: u32) -> RectF {
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
        let page = (track.h - self.row_height(dpi)).max(self.row_height(dpi));
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
        self.scroll = ratio * self.max_scroll_in(rect.h, dpi);
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

    /// Row rectangle in window coordinates.
    pub(super) fn row_rect(&self, rect: RectF, dpi: u32, index: usize) -> RectF {
        let row = self.row_height(dpi);
        rect_row(rect, self.header_height(dpi), self.scroll, row, index)
    }

    /// Draws header, rows, selection, the scrollbar and the empty state.
    pub(super) fn paint(&self, cv: &Canvas, rect: RectF, dpi: u32, palette: &Palette, icons: &[Option<ID2D1Bitmap>]) {
        let s = |value: f32| value * dpi as f32 / 96.0;
        let text = TextField { palette, dpi };
        if rect.w < s(40.0) || rect.h < s(20.0) { return; }
        let header = self.header_height(dpi);
        let header_rect = RectF::new(rect.x, rect.y, rect.w, header);
        // The header sits on its own translucent slab, like Explorer's column bar.
        cv.fill(header_rect, text.color(palette.hover));
        // A long folder keeps its own lane on the right, so no cell runs under
        // the thumb and grabbing the bar never selects a row by accident.
        let content = self.content_rect(rect, dpi);
        cv.push_clip(rect);
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
            let row = self.row_rect(content, dpi, index);
            if row.bottom() <= rect.y + header || row.y >= rect.bottom() { continue; }
            let selected = self.selection.contains(&index);
            let hovered = self.hover == Some(index);
            if selected {
                cv.round(RectF::new(row.x + s(2.0), row.y + 0.5, (row.w - s(4.0)).max(0.0), (row.h - 1.0).max(1.0)),
                    s(4.0), text.accent(palette.accent));
            } else if hovered {
                cv.round(RectF::new(row.x + s(2.0), row.y + 0.5, (row.w - s(4.0)).max(0.0), (row.h - 1.0).max(1.0)),
                    s(4.0), text.color(palette.hover));
            }
            let color = if selected { text.color(palette.text) } else { text.color(palette.text) };
            let Some(entry) = self.items.get(index) else { continue };
            let icon_rect = RectF::new(row.x + s(PAD), row.y + (row.h - s(ICON)) / 2.0, s(ICON), s(ICON));
            match icons.get(index - first) {
                Some(Some(bitmap)) => cv.image(bitmap, cell(icon_rect), 1.0),
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
                        let left = icon_rect.right() + s(6.0);
                        cv.text(&entry.name, RectF::new(left, row.y, (area.right() - left - s(6.0)).max(0.0), row.h),
                            &TextStyle::ui(s(12.5)), color);
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
            let message = self.error.clone().unwrap_or_else(|| if self.loaded { "此文件夹为空".into() } else { "正在读取…".into() });
            let style = TextStyle::ui(s(12.5));
            cv.text(&message, RectF::new(body.x, body.y, body.w, body.h),
                &style.align(Align::Center), text.color(palette.text_secondary));
        }
        cv.pop_clip();
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

fn rect_row(rect: RectF, header: f32, scroll: f32, row: f32, index: usize) -> RectF {
    RectF::new(rect.x, rect.y + header + index as f32 * row - scroll, rect.w, row)
}

/// The Shell reports display names, which drop a known extension or carry a
/// folder's `desktop.ini` rename while the row keeps the real file name.
fn same_name(entry: &str, shell: &str) -> bool {
    if entry.eq_ignore_ascii_case(shell) { return true; }
    std::path::Path::new(entry).file_stem()
        .is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case(shell))
}

fn sort_items(items: &mut [FolderEntry], key: SortKey, ascending: bool) {
    items.sort_by(|a, b| {
        // Folders first, like Explorer's default listing.
        let order = b.is_dir.cmp(&a.is_dir);
        let order = if order != std::cmp::Ordering::Equal { order } else {
            match key {
                SortKey::Name => zsdesk_core::model::view::natural_cmp(&a.name, &b.name),
                SortKey::Kind => zsdesk_core::model::view::natural_cmp(&a.kind(), &b.kind())
                    .then_with(|| zsdesk_core::model::view::natural_cmp(&a.name, &b.name)),
                SortKey::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0))
                    .then_with(|| zsdesk_core::model::view::natural_cmp(&a.name, &b.name)),
                SortKey::Modified => a.modified.unwrap_or(0).cmp(&b.modified.unwrap_or(0))
                    .then_with(|| zsdesk_core::model::view::natural_cmp(&a.name, &b.name)),
            }
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
            hidden: false, size, modified, icon: 0 }
    }

    #[test]
    fn sorting_keeps_folders_first_and_follows_natural_order() {
        let mut items = vec![entry("b.txt", false, Some(2), None), entry("a", true, None, None), entry("a10.txt", false, Some(3), None), entry("a2.txt", false, Some(1), None)];
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
    fn a_selection_matches_the_name_the_shell_reports() {
        // The Shell drops a known extension and a folder's desktop.ini rename.
        assert!(same_name("报告.txt", "报告"));
        assert!(same_name("桌面", "桌面"));
        assert!(same_name("Report.TXT", "report.txt"));
        assert!(!same_name("报告.txt", "报告.doc"));
        assert!(!same_name("a", "b"));
    }
}
