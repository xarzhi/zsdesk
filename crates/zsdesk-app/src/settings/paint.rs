//! Direct2D painting for the Fluent settings dialog.

use super::model::{self, Control, Layout, Row, Section, Target, BUTTON_SIZE, DETAIL_SIZE, TITLE_SIZE};
use zsdesk_ui::theme::Palette;
use zsdesk_windows::gfx::{Align, Canvas, Color, RectF, TextStyle};

fn c(color: zsdesk_ui::theme::Color) -> Color { Color::rgba8(color.r, color.g, color.b, color.a) }

/// Dialog surface: a touch darker than the cards in light, lighter in dark.
fn dialog_fill(dark: bool) -> Color {
    if dark { Color::rgba8(0x20, 0x20, 0x20, 0xFF) } else { Color::rgba8(0xF3, 0xF3, 0xF3, 0xFF) }
}
fn card_fill(dark: bool) -> Color {
    if dark { Color::rgba8(0x2C, 0x2C, 0x2C, 0xFF) } else { Color::rgba8(0xFF, 0xFF, 0xFF, 0xFF) }
}
fn control_fill(dark: bool) -> Color {
    if dark { Color::rgba8(0x3A, 0x3A, 0x3A, 0xFF) } else { Color::rgba8(0xFA, 0xFA, 0xFA, 0xFF) }
}
fn track_off(dark: bool) -> Color {
    if dark { Color::rgba8(0xFF, 0xFF, 0xFF, 0x24) } else { Color::rgba8(0, 0, 0, 0x0D) }
}

struct Scaler { dpi: u32 }
impl Scaler { fn s(&self, value: f32) -> f32 { model::scale(value, self.dpi) } }

pub struct Paint<'a> {
    pub canvas: &'a Canvas<'a>,
    pub layout: &'a Layout,
    pub sections: &'a [Section],
    pub section: usize,
    pub palette: Palette,
    pub dark: bool,
    pub active: bool,
    pub hover: Target,
    pub scroll: f32,
    pub message: Option<&'a str>,
}

impl Paint<'_> {
    pub fn draw(&self) {
        let cv = self.canvas;
        let l = self.layout;
        let s = Scaler { dpi: l.dpi };
        let p = &self.palette;
        let (width, height) = (l.width as f32, l.height as f32);
        cv.fill(RectF::new(0.0, 0.0, width, height), dialog_fill(self.dark));
        // Hairline keeps the popup readable on light and dark wallpapers alike.
        cv.stroke_round(RectF::new(0.5, 0.5, width - 1.0, height - 1.0), s.s(10.0), c(p.stroke), 1.0);
        self.header();
        self.navigation();
        cv.push_clip(l.view);
        self.content();
        cv.pop_clip();
        self.scrollbar();
        self.footer();
    }

    fn header(&self) {
        let cv = self.canvas;
        let l = self.layout;
        let s = Scaler { dpi: l.dpi };
        let p = &self.palette;
        cv.text("设置", RectF::new(l.title.x, s.s(6.0), l.title.w, s.s(30.0)), &TextStyle::ui(s.s(19.0)).weight(600), c(p.text));
        cv.text("ZSDesk 文件管理器", RectF::new(l.subtitle.x, s.s(33.0), l.subtitle.w, s.s(16.0)),
            &TextStyle::ui(s.s(11.0)), c(p.text_secondary));
        if self.hover == Target::Close { cv.round(l.close, s.s(4.0), c(p.hover)); }
        cv.glyph("\u{E8BB}", l.close, s.s(11.0), if self.active { c(p.text) } else { c(p.text_secondary) });
    }

    fn navigation(&self) {
        let cv = self.canvas;
        let l = self.layout;
        let s = Scaler { dpi: l.dpi };
        let p = &self.palette;
        for (index, rect) in l.nav.iter().enumerate() {
            let Some(section) = self.sections.get(index) else { continue };
            let selected = index == self.section;
            let hovered = matches!(self.hover, Target::Section(hover) if hover == index);
            if selected {
                cv.round(*rect, s.s(5.0), c(p.hover));
                // Windows 11 settings marks the active page with an accent pill.
                let bar = RectF::new(rect.x + s.s(2.0), rect.y + rect.h * 0.31, s.s(3.0), rect.h * 0.38);
                cv.round(bar, s.s(1.5), c(p.accent));
            } else if hovered {
                cv.round(*rect, s.s(5.0), c(p.hover).with_alpha(0.6));
            }
            let icon = RectF::new(rect.x + s.s(13.0), rect.y, s.s(18.0), rect.h);
            cv.glyph(section.icon, icon, s.s(13.0), if selected { c(p.text) } else { c(p.text_secondary) });
            let label = RectF::new(icon.right() + s.s(10.0), rect.y, (rect.right() - icon.right() - s.s(16.0)).max(0.0), rect.h);
            let style = TextStyle::ui(s.s(13.0)).weight(if selected { 600 } else { 400 });
            cv.text(section.title, label, &style, if selected { c(p.text) } else { c(p.text_secondary) });
        }
    }

    fn content(&self) {
        let cv = self.canvas;
        let l = self.layout;
        let s = Scaler { dpi: l.dpi };
        let p = &self.palette;
        let Some(section) = self.sections.get(self.section) else { return };
        let offset = l.view.y - self.scroll;
        cv.text(section.title, RectF::new(l.view.x + s.s(4.0), offset + s.s(10.0), l.view.w, s.s(30.0)),
            &TextStyle::ui(s.s(23.0)).weight(600), c(p.text));
        for (group_index, group) in l.groups.iter().enumerate() {
            let Some(model_group) = section.groups.get(group_index) else { continue };
            if let (Some(title), Some(label)) = (group.title, model_group.title) {
                let style = TextStyle::ui(s.s(12.0)).weight(600);
                cv.text(label, RectF::new(title.x, offset + title.y, title.w, title.h), &style, c(p.text_secondary));
            }
            let card = RectF::new(group.rect.x, offset + group.rect.y, group.rect.w, group.rect.h);
            cv.round(card, s.s(8.0), card_fill(self.dark));
            cv.stroke_round(card, s.s(8.0), c(p.stroke), 1.0);
            for (row_index, box_) in group.rows.iter().enumerate() {
                let Some(model_row) = model_group.rows.get(row_index) else { continue };
                if row_index > 0 {
                    let y = (offset + box_.rect.y).round() - 0.5;
                    cv.line(card.x + s.s(16.0), y, card.right() - s.s(16.0), y, c(p.divider), 1.0);
                }
                self.row(model_row, group_index, row_index, box_, offset);
            }
        }
    }

    fn row(&self, model_row: &Row, group: usize, index: usize, box_: &model::RowBox, offset: f32) {
        let cv = self.canvas;
        let s = Scaler { dpi: self.layout.dpi };
        let p = &self.palette;
        let left = box_.rect.x + s.s(16.0);
        let control = RectF::new(box_.control.x, offset + box_.control.y, box_.control.w, box_.control.h);
        let text_width = (control.x - s.s(12.0) - left).max(0.0);
        if model_row.detail.is_empty() {
            cv.text(&model_row.title, RectF::new(left, offset + box_.rect.y, text_width, box_.rect.h),
                &TextStyle::ui(s.s(TITLE_SIZE)), c(p.text));
        } else {
            cv.text(&model_row.title, RectF::new(left, offset + box_.rect.y + s.s(12.0), text_width, s.s(20.0)),
                &TextStyle::ui(s.s(TITLE_SIZE)), c(p.text));
            cv.text(&model_row.detail, RectF::new(left, offset + box_.rect.y + s.s(32.0), text_width, s.s(18.0)),
                &TextStyle::ui(s.s(DETAIL_SIZE)), c(p.text_secondary));
        }
        let hovered = |target: Target| self.hover == target;
        match &model_row.control {
            Control::Toggle { on, enabled } => self.toggle(control, *on, *enabled),
            Control::Segments => {
                cv.round(control, s.s(6.0), control_fill(self.dark));
                cv.stroke_round(control, s.s(6.0), c(p.stroke), 1.0);
                for (option, area) in box_.options.iter().enumerate() {
                    let rect = RectF::new(area.x, offset + area.y, area.w, area.h);
                    let selected = option == model_row.selected;
                    if selected {
                        cv.round(rect.inset(1.0, 1.0), s.s(4.0), c(p.accent));
                    } else if hovered(Target::Option { group, row: index, option }) {
                        cv.round(rect.inset(1.0, 1.0), s.s(4.0), c(p.hover));
                    }
                    let style = TextStyle::ui(s.s(12.0)).align(Align::Center).weight(if selected { 600 } else { 400 });
                    let color = if selected { c(p.accent_text) } else { c(p.text) };
                    cv.text(model_row.options.get(option).copied().unwrap_or(""), rect, &style, color);
                }
            }
            Control::Button { enabled } => {
                let row_hovered = hovered(Target::Row { group, row: index });
                let fill = if !enabled { control_fill(self.dark).with_alpha(0.45) }
                    else if row_hovered { c(p.hover) } else { control_fill(self.dark) };
                cv.round(control, s.s(4.0), fill);
                cv.stroke_round(control, s.s(4.0), c(p.stroke).with_alpha(if *enabled { 1.0 } else { 0.5 }), 1.0);
                cv.line(control.x + s.s(4.0), control.bottom() - 1.5, control.right() - s.s(4.0), control.bottom() - 1.5,
                    if self.dark { Color::rgba8(0, 0, 0, 0x40) } else { Color::rgba8(0, 0, 0, 0x10) }, 1.0);
                let style = TextStyle::ui(s.s(BUTTON_SIZE)).align(Align::Center);
                cv.text(&model_row.label, control, &style, if *enabled { c(p.text) } else { c(p.text_disabled) });
            }
            Control::Value => {
                cv.text(&model_row.label, control, &TextStyle::ui(s.s(12.5)).align(Align::End), c(p.text));
            }
        }
    }

    fn toggle(&self, rect: RectF, on: bool, enabled: bool) {
        let cv = self.canvas;
        let p = &self.palette;
        let radius = rect.h / 2.0;
        let knob = rect.h - 4.0;
        let knob_rect = |x: f32| RectF::new(x, rect.y + 2.0, knob, knob);
        if !enabled {
            cv.round(rect, radius, track_off(self.dark));
            cv.stroke_round(rect, radius, c(p.stroke), 1.0);
            let x = if on { rect.right() - knob - 2.0 } else { rect.x + 2.0 };
            cv.round(knob_rect(x), knob / 2.0, c(p.text_disabled));
            return;
        }
        if on {
            cv.round(rect, radius, c(p.accent));
            cv.round(knob_rect(rect.right() - knob - 2.0), knob / 2.0, c(p.accent_text));
        } else {
            cv.round(rect, radius, track_off(self.dark));
            cv.stroke_round(rect, radius, if self.dark { Color::rgba8(0xFF, 0xFF, 0xFF, 0x66) } else { Color::rgba8(0, 0, 0, 0x60) }, 1.0);
            cv.round(knob_rect(rect.x + 2.0), knob / 2.0,
                if self.dark { Color::rgba8(0xD6, 0xD6, 0xD6, 0xFF) } else { Color::rgba8(0x5A, 0x5A, 0x5A, 0xFF) });
        }
    }

    fn scrollbar(&self) {
        let Some(thumb) = self.layout.scrollbar else { return };
        let cv = self.canvas;
        let p = &self.palette;
        cv.round(thumb.thumb, thumb.thumb.w / 2.0, c(p.text_secondary).with_alpha(0.6));
    }

    fn footer(&self) {
        let Some(message) = self.message else { return };
        let cv = self.canvas;
        let l = self.layout;
        let s = Scaler { dpi: l.dpi };
        let p = &self.palette;
        let style = TextStyle::ui(s.s(11.5));
        let rect = RectF::new(l.view.x + s.s(4.0), l.height as f32 - s.s(24.0), l.view.w - s.s(16.0), s.s(18.0));
        cv.text(message, rect, &style, c(p.text_secondary));
    }
}
