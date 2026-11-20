//! Self-drawn navigation tree.
//!
//! The Shell namespace tree is a native `SysTreeView32`: it fills its own
//! opaque client area and its ClearType labels are blended against that fill.
//! Inside a window whose frame is extended over the whole client area the
//! control therefore shows up as a solid slab with ghosted text — no colour
//! message or theme trick changes that. This module keeps a flat list of
//! expanded rows and paints them with the same Direct2D surface as the rest of
//! the chrome, so the material and the wallpaper run edge to edge.
//!
//! The rows are plain data: a label, the location it opens, an icon index and
//! the expansion state. Children are enumerated lazily on the first expand.

use std::path::{Path, PathBuf};

use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use zsdesk_core::model::explorer::ShellLocation;
use zsdesk_ui::theme::{glyph, Mode, Palette};
use zsdesk_windows::{
    gfx::{Align, Canvas, Color, RectF, TextStyle},
    shell::folder_items::{self, FolderEntry},
};

/// Row height and horizontal metrics, in DIP.
const ROW_H: f32 = 26.0;
const INDENT: f32 = 16.0;
const ICON: f32 = 16.0;
const CHEVRON: f32 = 14.0;

#[derive(Clone, Debug)]
pub(super) struct NavRow {
    pub name: String,
    pub location: ShellLocation,
    /// System image-list index.
    pub icon: i32,
    pub depth: u8,
    /// Drawn with an expander; a folder that turned out to be empty loses it
    /// after the first expand.
    pub has_children: bool,
    pub expanded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NavHit { Row(usize), Chevron(usize), Empty }

#[derive(Default)]
pub(super) struct NavTree {
    pub rows: Vec<NavRow>,
    pub selection: Option<usize>,
    hover: Option<usize>,
    scroll: f32,
    width: i32,
    height: i32,
    dpi: u32,
    loaded: bool,
}

impl NavTree {
    pub(super) fn ensure_loaded(&mut self) {
        if self.loaded { return; }
        self.loaded = true;
        self.rows = folder_items::tree_roots().into_iter()
            .map(|(name, location, icon)| NavRow { name, location, icon, depth: 0, has_children: true, expanded: false })
            .collect();
    }

    pub(super) fn set_viewport(&mut self, width: i32, height: i32, dpi: u32) {
        self.width = width;
        self.height = height;
        self.dpi = dpi;
        self.ensure_loaded();
        self.clamp_scroll();
    }

    pub(super) fn row_height(&self) -> f32 { ROW_H * self.dpi.max(96) as f32 / 96.0 }
    fn s(&self, value: f32) -> f32 { value * self.dpi.max(96) as f32 / 96.0 }

    fn content_height(&self) -> f32 { self.rows.len() as f32 * self.row_height() }

    fn max_scroll(&self) -> f32 { (self.content_height() - self.height as f32).max(0.0) }

    fn clamp_scroll(&mut self) { self.scroll = self.scroll.clamp(0.0, self.max_scroll()); }

    /// First and one-past-last row that intersect the viewport.
    pub(super) fn visible_rows(&self) -> (usize, usize) {
        let row_h = self.row_height().max(1.0);
        let first = (self.scroll / row_h).floor().max(0.0) as usize;
        let count = (self.height as f32 / row_h).ceil() as usize + 2;
        (first.min(self.rows.len()), (first + count).min(self.rows.len()))
    }

    /// Row index at a client point, with the expander separated from the row.
    pub(super) fn hit(&self, x: f32, y: f32, rect: RectF) -> NavHit {
        if rect.w <= 0.0 || rect.h <= 0.0 || !rect.contains(x, y) { return NavHit::Empty; }
        let row_h = self.row_height().max(1.0);
        let offset = (y - rect.y + self.scroll) / row_h;
        if offset < 0.0 { return NavHit::Empty; }
        let index = offset.floor() as usize;
        let Some(row) = self.rows.get(index) else { return NavHit::Empty };
        let chevron_x = rect.x + self.s(6.0) + row.depth as f32 * self.s(INDENT);
        let chevron_y = rect.y + index as f32 * row_h - self.scroll;
        let chevron = RectF::new(chevron_x, chevron_y, self.s(CHEVRON), row_h);
        if row.has_children && chevron.contains(x, y) { NavHit::Chevron(index) } else { NavHit::Row(index) }
    }

    pub(super) fn set_hover(&mut self, index: Option<usize>) { self.hover = index; }
    pub(super) fn hover(&self) -> Option<usize> { self.hover }

    /// Moves the content by `delta` pixels: positive scrolls down the list, the
    /// same sign convention as the file view and the settings page. Callers turn
    /// a wheel notch into a *negative* delta for a notch away from the user,
    /// which then moves the view up instead of doubling the sign.
    pub(super) fn scroll_by(&mut self, delta: f32) {
        self.scroll += delta;
        self.clamp_scroll();
    }

    /// Expands or collapses the row, keeping the selection on the same folder.
    pub(super) fn toggle(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else { return };
        if row.expanded { self.collapse(index); } else { self.expand(index); }
    }

    pub(super) fn expand(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else { return };
        if row.expanded { return; }
        let depth = row.depth;
        let location = row.location.clone();
        let children = folder_items::folder_children(&location);
        if let Some(row) = self.rows.get_mut(index) {
            row.expanded = true;
            row.has_children = !children.is_empty();
        }
        let mut at = index + 1;
        for child in children {
            self.rows.insert(at, NavRow {
                name: child.name.clone(),
                location: location_of(&child),
                icon: child.icon,
                depth: depth.saturating_add(1),
                has_children: true,
                expanded: false,
            });
            at += 1;
        }
    }

    pub(super) fn collapse(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else { return };
        let depth = row.depth;
        self.rows[index].expanded = false;
        let mut end = index + 1;
        while end < self.rows.len() && self.rows[end].depth > depth { end += 1; }
        self.rows.drain(index + 1..end);
        // Indices shifted: re-find the row that was selected.
        let selected = self.selected_location();
        self.selection = selected.as_ref().and_then(|location| self.locate(location));
        self.hover = None;
        self.clamp_scroll();
    }

    /// Selects the row that shows `location`, expanding its ancestors first.
    pub(super) fn reveal(&mut self, location: &ShellLocation) {
        self.ensure_loaded();
        if let Some(index) = self.locate(location) {
            self.selection = Some(index);
            self.ensure_visible(index);
            return;
        }
        if let Some(path) = location.as_path() {
            let mut chain: Vec<PathBuf> = path.ancestors().skip(1).map(Path::to_path_buf).collect();
            chain.reverse();
            for ancestor in chain {
                let position = self.rows.iter().position(|row| row.location.as_path() == Some(ancestor.as_path()));
                let Some(index) = position else { continue };
                if !self.rows[index].expanded && self.rows[index].has_children { self.expand(index); }
            }
        }
        self.selection = self.locate(location);
        if let Some(index) = self.selection { self.ensure_visible(index); }
    }

    pub(super) fn selected_location(&self) -> Option<ShellLocation> {
        self.selection.and_then(|index| self.rows.get(index)).map(|row| row.location.clone())
    }

    fn locate(&self, location: &ShellLocation) -> Option<usize> {
        let address = location.address();
        self.rows.iter().position(|row| match (location.as_path(), row.location.as_path()) {
            (Some(a), Some(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
            _ => row.location.address().eq_ignore_ascii_case(&address),
        })
    }

    fn ensure_visible(&mut self, index: usize) {
        let row_h = self.row_height().max(1.0);
        let top = index as f32 * row_h;
        let bottom = top + row_h;
        if top < self.scroll { self.scroll = top; }
        else if bottom > self.scroll + self.height as f32 { self.scroll = bottom - self.height as f32; }
        self.clamp_scroll();
    }

    /// Moves the selection by `delta` rows.
    pub(super) fn step(&mut self, delta: isize) -> Option<ShellLocation> {
        let Some(current) = self.selection else {
            let index = if delta > 0 { 0 } else { self.rows.len().saturating_sub(1) };
            self.selection = (!self.rows.is_empty()).then_some(index);
            return self.selected_location();
        };
        if self.rows.is_empty() { return None; }
        let next = (current as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize;
        self.selection = Some(next);
        self.ensure_visible(next);
        self.selected_location()
    }

    pub(super) fn paint(&self, cv: &Canvas, rect: RectF, palette: &Palette, material: bool,
        icons: &[Option<ID2D1Bitmap>], first: usize) {
        if rect.w < 20.0 || rect.h < 10.0 { return; }
        let s = |value: f32| value * self.dpi.max(96) as f32 / 96.0;
        let row_h = self.row_height().max(1.0);
        // A faint veil keeps the column readable over a bright wallpaper while
        // still letting it through; without a material the column is solid.
        let base = if material {
            let v = palette.veil(0.06);
            Color::rgba8(v.r, v.g, v.b, v.a)
        } else { Color::rgba8(palette.layer.r, palette.layer.g, palette.layer.b, 0xFF) };
        cv.fill(rect, base);
        cv.push_clip(rect);
        for index in first..self.visible_rows().1 {
            let row = &self.rows[index];
            let y = rect.y + index as f32 * row_h - self.scroll;
            let row_rect = RectF::new(rect.x, y, rect.w, row_h);
            if row_rect.bottom() <= rect.y || row_rect.y >= rect.bottom() { continue; }
            let selected = self.selection == Some(index);
            let hovered = self.hover == Some(index);
            let chip = RectF::new(rect.x + s(4.0), y + 0.5, (rect.w - s(8.0)).max(0.0), (row_h - 1.0).max(1.0));
            if selected {
                cv.round(chip, s(4.0), Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0x54));
            } else if hovered {
                cv.round(chip, s(4.0), Color::rgba8(palette.hover.r, palette.hover.g, palette.hover.b, palette.hover.a));
            }
            let text = Color::rgba8(palette.text.r, palette.text.g, palette.text.b, 0xFF);
            let left = rect.x + s(6.0) + row.depth as f32 * s(INDENT);
            if row.has_children {
                let chevron = RectF::new(left, y, s(CHEVRON), row_h);
                cv.glyph(if row.expanded { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT }, chevron, s(9.0),
                    Color::rgba8(palette.text_secondary.r, palette.text_secondary.g, palette.text_secondary.b, 0xFF));
            }
            let icon_rect = RectF::new(left + s(CHEVRON) + s(2.0), y + (row_h - s(ICON)) / 2.0, s(ICON), s(ICON));
            match icons.get(index - first).and_then(|icon| icon.as_ref()) {
                Some(bitmap) => cv.image(bitmap, RectF::new(icon_rect.x.round(), icon_rect.y.round(), icon_rect.w.round(), icon_rect.h.round()), 1.0),
                None => cv.glyph(glyph::FOLDER, icon_rect, s(14.0), text),
            }
            let text_left = icon_rect.right() + s(6.0);
            cv.text(&row.name, RectF::new(text_left, y, (rect.right() - text_left - s(8.0)).max(0.0), row_h),
                &TextStyle::ui(s(12.5)).align(Align::Start), text);
        }
        cv.pop_clip();
        // A hairline thumb tells the reader there is more below; the wheel and
        // the arrow keys do the scrolling. A neutral grey keeps it readable on
        // both modes: the text colour is pure black/white and would read as a
        // dirty bar once it is blended over a wallpaper.
        if self.content_height() > rect.h {
            let thumb = (rect.h / self.content_height() * rect.h).clamp(s(24.0), rect.h);
            let travel = (rect.h - thumb).max(0.0);
            let y = rect.y + (self.scroll / self.max_scroll().max(1.0)) * travel;
            let tint = if palette.mode == Mode::Dark { (0xFF, 0xFF, 0xFF, 0x33) } else { (0x00, 0x00, 0x00, 0x26) };
            cv.round(RectF::new(rect.right() - s(6.0), y, s(3.0), thumb), s(1.5),
                Color::rgba8(tint.0, tint.1, tint.2, tint.3));
        }
        // Windows 11 divides the panes with the slab edge alone; a dark hairline
        // over a wallpaper reads as a stray black border, so the rule keeps the
        // palette's own alpha (a few percent) instead of being forced opaque.
        let divider = palette.divider;
        cv.line(rect.right() - 0.5, rect.y, rect.right() - 0.5, rect.bottom(),
            Color::rgba8(divider.r, divider.g, divider.b, divider.a), 1.0);
    }
}

/// The location a listed folder opens.
fn location_of(entry: &FolderEntry) -> ShellLocation {
    match entry.path.clone() {
        Some(path) => ShellLocation::Path(path),
        None => ShellLocation::Shell { parsing_name: entry.parsing_name.clone(), title: entry.name.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, depth: u8) -> NavRow {
        NavRow { name: name.into(), location: ShellLocation::Path(PathBuf::from(name)), icon: 0, depth, has_children: true, expanded: false }
    }

    #[test]
    fn collapse_removes_only_the_children_below_the_row() {
        let mut tree = NavTree { rows: vec![row("a", 0), row("b", 1), row("c", 2), row("d", 1), row("e", 0)], loaded: true, ..Default::default() };
        tree.rows[0].expanded = true;
        tree.collapse(0);
        assert_eq!(tree.rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["a", "e"]);
    }

    #[test]
    fn hit_separates_the_expander_from_the_label() {
        let mut tree = NavTree { rows: vec![row("a", 0)], dpi: 96, loaded: true, ..Default::default() };
        tree.set_viewport(200, 200, 96);
        let rect = RectF::new(0.0, 0.0, 200.0, 200.0);
        assert_eq!(tree.hit(10.0, 10.0, rect), NavHit::Chevron(0));
        assert_eq!(tree.hit(80.0, 10.0, rect), NavHit::Row(0));
        assert_eq!(tree.hit(80.0, 300.0, rect), NavHit::Empty);
    }

    #[test]
    fn scrolling_is_clamped_to_the_content() {
        let mut tree = NavTree { rows: (0..40).map(|i| row(&format!("r{i}"), 0)).collect(), dpi: 96, loaded: true, ..Default::default() };
        tree.set_viewport(200, 100, 96);
        // Positive moves the content up into view; negative rewinds to the top.
        tree.scroll_by(10000.0);
        assert_eq!(tree.scroll, tree.max_scroll());
        tree.scroll_by(-10000.0);
        assert_eq!(tree.scroll, 0.0);
    }

    #[test]
    fn a_wheel_notch_away_from_the_user_scrolls_up() {
        let mut tree = NavTree { rows: (0..40).map(|i| row(&format!("r{i}"), 0)).collect(), dpi: 96, loaded: true, ..Default::default() };
        tree.set_viewport(200, 100, 96);
        tree.scroll = tree.max_scroll();
        // A notch away from the user reports +120; the caller negates it, and
        // the content must then move up (towards the first row).
        tree.scroll_by(-120.0 / 120.0 * tree.row_height() * 3.0);
        assert!(tree.scroll < tree.max_scroll(), "wheel up must reveal earlier rows");
        let after_up = tree.scroll;
        tree.scroll_by(120.0 / 120.0 * tree.row_height() * 3.0);
        assert!(tree.scroll > after_up, "wheel down must reveal later rows");
    }
}
