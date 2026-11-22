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
    /// Folder the row opens; `None` on a separator line.
    pub location: Option<ShellLocation>,
    /// System image-list index.
    pub icon: i32,
    pub depth: u8,
    /// Drawn with an expander; a folder that turned out to be empty loses it
    /// after the first expand.
    pub has_children: bool,
    pub expanded: bool,
    /// A rule between groups (主文件夹/图库, 快速访问, 此电脑), as in Explorer.
    pub separator: bool,
    /// Identity inside the namespace the row came from: a pinned folder of the
    /// quick-access group keeps its own item, so its menu is the one Explorer
    /// shows there (取消固定 instead of 固定).
    pub pidl: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NavHit { Row(usize), Chevron(usize), Empty }

#[derive(Default)]
pub(super) struct NavTree {
    pub rows: Vec<NavRow>,
    pub selection: Option<usize>,
    hover: Option<usize>,
    /// Row a drag would drop into, outlined while the drag hovers over it.
    drop_target: Option<usize>,
    scroll: f32,
    width: i32,
    height: i32,
    dpi: u32,
    loaded: bool,
    /// Identity of the quick-access group the rows were built from, so a
    /// refresh can skip rebuilding the pane when nothing was pinned.
    signature: Vec<String>,
}

impl NavTree {
    pub(super) fn ensure_loaded(&mut self) {
        if self.loaded { return; }
        self.loaded = true;
        self.signature = folder_items::quick_access_signature();
        self.build_roots();
        // 此电脑 opens with its drives in view, the way Explorer shows the pane
        // after the first launch: the letters are what the user came for.
        if let Some(index) = self.rows.iter().position(|row| row.location.as_ref() == Some(&ShellLocation::ThisPc)) {
            self.expand(index);
        }
    }

    /// Builds the root rows: 主文件夹, 图库, the pinned quick-access folders,
    /// 此电脑 and 网络.
    fn build_roots(&mut self) {
        self.rows = folder_items::tree_roots().into_iter()
            .map(|row| NavRow {
                separator: row.location.is_none(),
                name: row.name,
                location: row.location,
                icon: row.icon,
                depth: 0,
                has_children: true,
                expanded: false,
                pidl: row.pidl,
            })
            .collect();
    }

    /// Re-reads the pinned quick-access folders from the Shell.
    ///
    /// The roots are built once at start-up, so a pin or an unpin would
    /// otherwise keep the old rows until the window was reopened. Only the
    /// roots are rebuilt: every branch that was open is opened again, and the
    /// selection follows its folder, so refreshing never moves the user.
    /// Returns whether anything was rebuilt.
    pub(super) fn reload(&mut self) -> bool {
        if !self.loaded { self.ensure_loaded(); return true; }
        let signature = folder_items::quick_access_signature();
        if signature == self.signature { return false; }
        self.signature = signature;
        let expanded = self.expanded_locations();
        let selected = self.selected_location();
        self.build_roots();
        self.restore_expansion(&expanded);
        self.selection = selected.as_ref().and_then(|location| self.locate(location));
        self.clamp_scroll();
        true
    }

    /// Addresses of the rows that were unfolded, so a rebuild can open them
    /// again instead of folding the whole pane back to its roots.
    fn expanded_locations(&self) -> Vec<String> {
        self.rows.iter().filter(|row| row.expanded)
            .filter_map(|row| row.location.as_ref().map(ShellLocation::address)).collect()
    }

    /// Re-opens the branches of a freshly built root list.
    fn restore_expansion(&mut self, expanded: &[String]) {
        let mut index = 0;
        while index < self.rows.len() {
            let reopen = self.rows[index].location.as_ref()
                .is_some_and(|location| expanded.iter().any(|open| open.eq_ignore_ascii_case(&location.address())));
            if reopen { self.expand(index); }
            index += 1;
        }
        self.hover = None;
        self.drop_target = None;
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
        // A rule is not a target: clicking the gap does nothing instead of
        // navigating to whichever folder happens to sit next to it.
        if row.separator { return NavHit::Empty; }
        let chevron_x = rect.x + self.s(6.0) + row.depth as f32 * self.s(INDENT);
        let chevron_y = rect.y + index as f32 * row_h - self.scroll;
        let chevron = RectF::new(chevron_x, chevron_y, self.s(CHEVRON), row_h);
        if row.has_children && chevron.contains(x, y) { NavHit::Chevron(index) } else { NavHit::Row(index) }
    }

    pub(super) fn set_hover(&mut self, index: Option<usize>) { self.hover = index; }
    pub(super) fn hover(&self) -> Option<usize> { self.hover }

    /// Where the label of one row starts, so the rename editor can sit on it.
    pub(super) fn label_rect(&self, rect: RectF, index: usize) -> Option<RectF> {
        let row = self.rows.get(index)?;
        let row_h = self.row_height().max(1.0);
        let y = rect.y + index as f32 * row_h - self.scroll;
        let left = rect.x + self.s(6.0) + row.depth as f32 * self.s(INDENT)
            + self.s(CHEVRON) + self.s(2.0) + self.s(ICON) + self.s(6.0);
        Some(RectF::new(left, y, (rect.right() - left - self.s(6.0)).max(0.0), row_h))
    }

    /// Location a row opens, for a drop or a reveal.
    pub(super) fn location_of(&self, index: usize) -> Option<ShellLocation> {
        self.rows.get(index).and_then(|row| row.location.clone())
    }

    /// Item identity a row's context menu must describe, when the namespace it
    /// came from has one. A pinned quick-access folder only offers
    /// "从快速访问取消固定" through its own item, not through its path.
    pub(super) fn pidl_of(&self, index: usize) -> Option<Vec<u8>> {
        self.rows.get(index).and_then(|row| row.pidl.clone())
    }

    /// Row a drag would drop into; `None` clears the outline.
    pub(super) fn set_drop_target(&mut self, row: Option<usize>) -> bool {
        if self.drop_target == row { return false; }
        self.drop_target = row;
        true
    }

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
        if row.expanded || row.separator { return; }
        let depth = row.depth;
        let Some(location) = row.location.clone() else { return };
        let children = folder_items::folder_children(&location);
        if let Some(row) = self.rows.get_mut(index) {
            row.expanded = true;
            row.has_children = !children.is_empty();
        }
        let mut at = index + 1;
        for child in children {
            self.rows.insert(at, NavRow {
                name: child.name.clone(),
                location: Some(location_of(&child)),
                icon: child.icon,
                depth: depth.saturating_add(1),
                has_children: true,
                expanded: false,
                separator: false,
                pidl: None,
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
                let position = self.rows.iter().position(|row| {
                    row.location.as_ref().and_then(|location| location.as_path()) == Some(ancestor.as_path())
                });
                let Some(index) = position else { continue };
                if !self.rows[index].expanded && self.rows[index].has_children { self.expand(index); }
            }
        }
        self.selection = self.locate(location);
        if let Some(index) = self.selection { self.ensure_visible(index); }
    }

    pub(super) fn selected_location(&self) -> Option<ShellLocation> {
        self.selection.and_then(|index| self.rows.get(index)).and_then(|row| row.location.clone())
    }

    fn locate(&self, location: &ShellLocation) -> Option<usize> {
        let address = location.address();
        self.rows.iter().position(|row| {
            let Some(candidate) = row.location.as_ref() else { return false };
            match (location.as_path(), candidate.as_path()) {
                (Some(a), Some(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
                _ => candidate.address().eq_ignore_ascii_case(&address),
            }
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

    /// Moves the selection by `delta` rows, stepping over the group rules.
    pub(super) fn step(&mut self, delta: isize) -> Option<ShellLocation> {
        if self.rows.is_empty() { return None; }
        let Some(current) = self.selection else {
            let start = if delta > 0 { 0 } else { self.rows.len() - 1 };
            let index = self.next_row(start as isize, delta).unwrap_or(start);
            self.selection = Some(index);
            return self.selected_location();
        };
        let next = self.next_row(current as isize + delta, delta).unwrap_or(current);
        self.selection = Some(next);
        self.ensure_visible(next);
        self.selected_location()
    }

    /// First selectable row from `start` in `direction`, or `None` at the end.
    fn next_row(&self, start: isize, direction: isize) -> Option<usize> {
        let direction = if direction < 0 { -1 } else { 1 };
        let mut index = start;
        while (0..self.rows.len() as isize).contains(&index) {
            if !self.rows[index as usize].separator { return Some(index as usize); }
            index += direction;
        }
        None
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
            if row.separator {
                // Explorer divides the pane with a hairline rule between the
                // groups; the row itself carries no label and no hover state.
                let line = if palette.mode == Mode::Dark {
                    Color::rgba8(0xFF, 0xFF, 0xFF, 0x2b)
                } else { Color::rgba8(0x00, 0x00, 0x00, 0x1f) };
                let middle = y + row_h / 2.0;
                cv.line(rect.x + s(12.0), middle, rect.right() - s(12.0), middle, line, 1.0);
                continue;
            }
            let selected = self.selection == Some(index);
            let hovered = self.hover == Some(index);
            let targeted = self.drop_target == Some(index);
            let chip = RectF::new(rect.x + s(4.0), y + 0.5, (rect.w - s(8.0)).max(0.0), (row_h - 1.0).max(1.0));
            if targeted {
                cv.round(chip, s(4.0), Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0x2e));
                cv.stroke_round(chip, s(4.0), Color::rgba8(palette.accent.r, palette.accent.g, palette.accent.b, 0xB0), s(1.0));
            } else if selected {
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
        NavRow { name: name.into(), location: Some(ShellLocation::Path(PathBuf::from(name))), icon: 0, depth, has_children: true, expanded: false, separator: false, pidl: None }
    }

    fn rule() -> NavRow {
        NavRow { name: String::new(), location: None, icon: 0, depth: 0, has_children: false, expanded: false, separator: true, pidl: None }
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
    fn the_arrow_keys_step_over_group_rules() {
        let mut tree = NavTree { rows: vec![row("a", 0), rule(), row("b", 0)], dpi: 96, loaded: true, ..Default::default() };
        tree.set_viewport(200, 200, 96);
        tree.selection = Some(0);
        assert_eq!(tree.step(1).as_ref().and_then(ShellLocation::as_path).map(|path| path.to_string_lossy().into_owned()), Some("b".to_string()));
        assert_eq!(tree.step(-1).and_then(|location| location.as_path().map(|path| path.to_string_lossy().into_owned())), Some("a".to_string()));
        // A rule is never a click target either.
        let rect = RectF::new(0.0, 0.0, 200.0, 200.0);
        assert_eq!(tree.hit(80.0, 26.0 + 10.0, rect), NavHit::Empty);
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

    #[test]
    fn a_rebuild_reopens_the_branches_that_were_open() {
        let mut tree = NavTree { rows: vec![row("a", 0), rule(), row("b", 0)], dpi: 96, loaded: true, ..Default::default() };
        tree.hover = Some(2);
        tree.restore_expansion(&["a".to_string()]);
        assert!(tree.rows[0].expanded, "a branch that was open must open again");
        assert!(!tree.rows[2].expanded, "a branch that was closed must stay closed");
        assert_eq!(tree.hover, None, "a stale hover index would point at another row");
    }
}
