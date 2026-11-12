//! Content and pixel layout of the Fluent settings dialog.
//!
//! Sections, rows and their rectangles are pure functions of the saved
//! preferences, so the dialog can be laid out and hit-tested without a window.

use zsdesk_core::model::explorer::{
    BackdropStyle, ColorScheme, ContextMenuMode, ContextMenuPreferences, ExplorerUi, OpenMode,
    PreviewProviderPreference,
};
use zsdesk_windows::gfx::RectF;

/// Dialog size in DIP.
pub const DIALOG_W: f32 = 940.0;
pub const DIALOG_H: f32 = 660.0;

const HEADER_H: f32 = 54.0;
const NAV_W: f32 = 208.0;
const NAV_ITEM_H: f32 = 40.0;
const NAV_INSET: f32 = 10.0;
const CARD_RADIUS: f32 = 8.0;
const ROW_PAD_X: f32 = 16.0;
const CARD_GAP: f32 = 10.0;
const GROUP_TITLE_H: f32 = 30.0;
const TITLE_H: f32 = 62.0;
const SCROLLBAR_W: f32 = 3.0;

pub fn scale(value: f32, dpi: u32) -> f32 { value * dpi.max(96) as f32 / 96.0 }

/// What the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    None,
    Close,
    Section(usize),
    Row { group: usize, row: usize },
    Option { group: usize, row: usize, option: usize },
    Scrollbar,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Toggle { on: bool, enabled: bool },
    /// Radio-style segmented control; the options live in [`Row::options`].
    Segments,
    /// Drop-down list; closed it shows the selected option, open it floats a
    /// list over the cards below.
    Select { open: bool, enabled: bool },
    Button { enabled: bool },
    /// Read-only text (version numbers).
    Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: &'static str,
    pub title: String,
    pub detail: String,
    pub control: Control,
    pub options: Vec<&'static str>,
    /// One action id per option; toggles and buttons have exactly one.
    pub actions: Vec<&'static str>,
    /// Selected option of a segmented control.
    pub selected: usize,
    /// Text drawn inside a button.
    pub label: String,
}

impl Row {
    pub fn toggle(id: &'static str, title: &str, detail: &str, on: bool) -> Self {
        Self { id, title: title.into(), detail: detail.into(), control: Control::Toggle { on, enabled: true },
            options: Vec::new(), actions: vec![id], selected: 0, label: String::new() }
    }
    pub fn toggle_locked(id: &'static str, title: &str, detail: &str, on: bool, enabled: bool) -> Self {
        Self { control: Control::Toggle { on, enabled }, ..Self::toggle(id, title, detail, on) }
    }
    pub fn segments(id: &'static str, title: &str, detail: &str, options: &[&'static str], selected: usize, actions: &[&'static str]) -> Self {
        debug_assert_eq!(options.len(), actions.len());
        Self { id, title: title.into(), detail: detail.into(), control: Control::Segments,
            options: options.to_vec(), actions: actions.to_vec(), selected, label: String::new() }
    }
    pub fn button(id: &'static str, title: &str, detail: &str, label: &str) -> Self {
        Self { id, title: title.into(), detail: detail.into(), control: Control::Button { enabled: true },
            options: Vec::new(), actions: vec![id], selected: 0, label: label.into() }
    }
    pub fn select(id: &'static str, title: &str, detail: &str, options: &[&'static str], selected: usize, actions: &[&'static str]) -> Self {
        debug_assert_eq!(options.len(), actions.len());
        Self { id, title: title.into(), detail: detail.into(), control: Control::Select { open: false, enabled: true },
            options: options.to_vec(), actions: actions.to_vec(), selected: selected.min(options.len().saturating_sub(1)), label: String::new() }
    }
    /// Marks the drop-down as open; the dialog owns that transient state.
    pub fn opened(mut self, open: bool) -> Self {
        if let Control::Select { open: flag, .. } = &mut self.control { *flag = open; }
        self
    }
    pub fn button_locked(id: &'static str, title: &str, detail: &str, label: &str, enabled: bool) -> Self {
        Self { control: Control::Button { enabled }, ..Self::button(id, title, detail, label) }
    }
    pub fn value(id: &'static str, title: &str, value: &str) -> Self {
        Self { id, title: title.into(), detail: String::new(), control: Control::Value,
            options: Vec::new(), actions: Vec::new(), selected: 0, label: value.into() }
    }
    /// Action id triggered by clicking the control (or one of its options).
    pub fn action(&self, option: Option<usize>) -> Option<&'static str> {
        match (&self.control, option) {
            (Control::Segments | Control::Select { .. }, Some(index)) => self.actions.get(index).copied(),
            (Control::Segments | Control::Select { .. }, None) => self.actions.get(self.selected).copied(),
            (Control::Value, _) => None,
            (_, _) => self.actions.first().copied(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group { pub title: Option<&'static str>, pub rows: Vec<Row> }

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub groups: Vec<Group>,
}

/// Everything the dialog needs to describe the current preferences.
pub struct Snapshot<'a> {
    pub ui: &'a ExplorerUi,
    /// 0 = 不接管, 1 = 右键菜单, 2 = 默认打开.
    pub integration: u8,
    pub themed: bool,
    /// Id of the drop-down that is currently expanded, if any.
    pub open: Option<&'a str>,
}

fn group(title: Option<&'static str>, rows: Vec<Row>) -> Group { Group { title, rows } }

/// Builds every section for the current state.
pub fn build(snapshot: &Snapshot<'_>) -> Vec<Section> {
    let ui = snapshot.ui;
    let mut sections = vec![
        Section { id: "general", title: "常规", icon: "\u{E80F}", groups: vec![group(None, vec![
            Row::select("open-mode", "多标签的打开方式", "从其他窗口打开文件夹时，新建一个窗口或在本窗口新增标签页",
                &["新窗口打开", "在本窗口的新增标签页打开"],
                usize::from(ui.open_mode == OpenMode::NewTab),
                &["open-mode-window", "open-mode-tab"]).opened(snapshot.open == Some("open-mode")),
            Row::toggle("autostart", "开机自启", "登录 Windows 时在后台就绪，不弹出窗口", ui.autostart),
            Row::button("new-window", "新建窗口", "再开一个独立的浏览窗口，按需级联出现", "打开"),
            Row::button("quick-search", "窗口内搜索", "在地址栏右侧搜索当前文件夹（Ctrl+F）", "搜索"),
            Row::button("bookmark", "收藏当前文件夹", "把当前文件夹加入收藏栏（Ctrl+D）", "收藏"),
        ])] },
    ];
    if snapshot.themed {
        let appearance = &ui.appearance;
        let scheme = match appearance.color_scheme { ColorScheme::System => 0, ColorScheme::Light => 1, ColorScheme::Dark => 2 };
        let material = match appearance.backdrop {
            BackdropStyle::Acrylic => 0, BackdropStyle::Mica => 1, BackdropStyle::Solid => 2, BackdropStyle::System => 1,
        };
        sections.push(Section { id: "appearance", title: "外观", icon: "\u{E790}", groups: vec![group(Some("主题"), vec![
            Row::segments("appearance-scheme", "颜色模式", "跟随 Windows 设置，或固定为浅色／深色",
                &["跟随系统", "浅色", "深色"], scheme,
                &["appearance-color-system", "appearance-color-light", "appearance-color-dark"]),
            Row::segments("appearance-material", "窗口材质", "亚克力与 Mica 需要 Windows 11 22H2 及以后版本",
                &["亚克力", "Mica", "纯色"], material,
                &["appearance-acrylic", "appearance-mica", "appearance-solid"]),
        ])] });
    }
    let expand = match ui.navigation_auto_expand { None => 0, Some(true) => 1, Some(false) => 2 };
    let provider = match ui.preview_provider {
        PreviewProviderPreference::System => 0, PreviewProviderPreference::Office => 1, PreviewProviderPreference::Wps => 2 };
    sections.push(Section { id: "interface", title: "界面", icon: "\u{E8A9}", groups: vec![
        group(Some("布局"), vec![
            Row::toggle("interface-navigation", "导航窗格", "在窗口左侧显示文件夹树", ui.show_tree),
            Row::segments("navigation-auto-expand", "展开到当前文件夹", "导航窗格跟随当前位置展开，或保持手动展开",
                &["跟随系统", "总是展开", "不展开"], expand,
                &["navigation-expand-system", "navigation-expand-always", "navigation-expand-never"]),
            Row::toggle("interface-bookmarks", "收藏栏", "在地址栏下方显示收藏的文件夹", ui.show_bookmarks),
            Row::toggle("command-bar-visible", "命令栏", "显示新建、剪切、复制等常用命令", ui.command_bar.visible),
        ]),
        group(Some("预览"), vec![
            Row::toggle("space-preview", "空格快速预览", "选中文件后按空格键预览内容", ui.space_preview),
            Row::toggle("hover-preview", "悬停预览", "鼠标停在文件上时显示缩略内容", ui.hover_preview),
            Row::segments("preview-provider", "预览插件优先级", "系统预览与 Office／WPS 组件都可以打开时先尝试哪一个",
                &["系统默认", "Office 优先", "WPS 优先"], provider,
                &["preview-provider-system", "preview-provider-office", "preview-provider-wps"]),
        ]),
    ] });
    let takeover = (snapshot.integration.min(2) as usize).min(2);
    let owned = snapshot.integration == 2;
    let recycle = ui.recycle_bin_takeover;
    sections.push(Section { id: "integration", title: "系统集成", icon: "\u{E713}", groups: vec![
        group(Some("接管方式"), vec![
            Row::segments("integration-level", "文件夹与磁盘", "只影响当前用户，修改前会自动备份原有设置",
                &["不接管", "右键菜单", "默认打开"], takeover,
                &["integration-off", "integration-context", "integration-default"]),
            Row::toggle_locked("integration-recycle", "接管桌面回收站", "双击桌面“回收站”时由 ZSDesk 打开",
                recycle, owned),
            Row::toggle_locked("integration-forward", "接管新出现的系统浏览窗口",
                "资源管理器自己打开的文件夹窗口立即转由 ZSDesk 打开", ui.forward_unhandled_windows, owned),
        ]),
        group(Some("维护"), vec![
            Row::button_locked("integration-restore", "恢复系统资源管理器", "撤销全部接管，还原为本程序安装前的设置",
                "立即恢复", snapshot.integration != 0),
            Row::button("routing-settings", "文件位置转发…", "让其他程序请求的文件夹路径也交给 ZSDesk", "配置"),
        ]),
    ] });
    let context = &ui.context_menu;
    let modern = context.mode == ContextMenuMode::Windows11;
    sections.push(Section { id: "context", title: "右键菜单", icon: "\u{E700}", groups: vec![
        group(Some("样式"), vec![
            Row::segments("context-mode", "系统扩展菜单样式", "Windows 11 原生菜单更简洁，经典菜单显示全部扩展",
                &["经典系统菜单", "Windows 11 原生菜单"], usize::from(modern),
                &["context-menu-original", "context-menu-windows11"]),
        ]),
        group(Some("菜单功能"), context_rows(context)),
        group(None, vec![
            Row::button("context-menu-defaults", "恢复默认右键设置", "重新显示全部菜单功能项", "恢复默认"),
        ]),
    ] });
    let everything = ui.everything_executable.as_ref().map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "未选择，搜索会退回本程序自带的索引".into());
    sections.push(Section { id: "search", title: "搜索", icon: "\u{E721}", groups: vec![
        group(Some("范围"), vec![
            Row::segments("search-scope", "默认搜索范围", "按 Ctrl+F 时搜索全部磁盘，或只搜索当前文件夹",
                &["全部本地磁盘", "仅当前文件夹"], usize::from(!ui.search_all_files),
                &["search-all-files", "search-current-folder"]),
        ]),
        group(Some("Everything"), vec![
            Row::button("search-everything-program", "已安装的 Everything", &everything, "选择"),
            Row::button("search-index-settings", "Everything 索引设置…", "在 Everything 中调整索引的磁盘与文件夹", "打开"),
            Row::button("search-everything-service", "启用快速索引服务…", "安装并启动 Everything 后台服务", "启用"),
            Row::button("search-rebuild-index", "重建文件名索引", "文件列表与搜索结果与实际内容不一致时使用", "重建"),
        ]),
        group(Some("快捷键"), vec![
            Row::button("quick-search-settings", "全局搜索快捷键…", "设置唤出窗口的快捷键与登录启动", "设置"),
        ]),
    ] });
    sections.push(Section { id: "about", title: "关于", icon: "\u{E946}", groups: vec![
        group(Some("ZSDesk"), vec![
            Row::value("version", "版本", env!("CARGO_PKG_VERSION")),
            Row::value("edition", "版本类型", crate::edition::name()),
            Row::value("shell", "内容区", "系统 Shell 视图 · ZSUI Fluent 界面"),
        ]),
        group(Some("帮助"), vec![
            Row::button("shortcuts", "键盘快捷键", "查看文件管理器的全部快捷键", "查看"),
            Row::button("folder-options", "文件夹选项", "打开系统“文件夹选项”对话框", "打开"),
            Row::button("about", "关于对话框", "版本与组件信息", "查看"),
        ]),
    ] });
    sections
}

fn context_rows(options: &ContextMenuPreferences) -> Vec<Row> {
    let flags = [
        ("context-action-cut-copy-paste", "剪切、复制和粘贴", options.show_cut_copy_paste),
        ("context-action-copy-path", "复制路径", options.show_copy_path),
        ("context-action-rename", "重命名", options.show_rename),
        ("context-action-delete", "删除", options.show_delete),
        ("context-action-properties", "属性", options.show_properties),
        ("context-action-share", "共享", options.show_share),
        ("context-action-new", "新建", options.show_new),
        ("context-action-seven-zip", "7-Zip", options.show_seven_zip),
        ("context-action-wps", "WPS", options.show_wps),
    ];
    flags.iter().map(|(id, label, on)| Row::toggle(id, label, "", *on)).collect()
}

/// Rows of one card.
pub struct GroupBox { pub rect: RectF, pub rows: Vec<RowBox>, pub title: Option<RectF> }

pub struct RowBox {
    pub rect: RectF,
    pub control: RectF,
    pub options: Vec<RectF>,
    pub label: Option<RectF>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Thumb { pub track: RectF, pub thumb: RectF }

/// Expanded drop-down list. The rectangles are in window coordinates because
/// the list floats over the cards and is not clipped by the scroll view.
#[derive(Clone, Debug, Default)]
pub struct SelectPopup {
    pub owner: (usize, usize),
    pub rect: RectF,
    pub options: Vec<RectF>,
}

pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub dpi: u32,
    pub header: RectF,
    pub close: RectF,
    pub title: RectF,
    pub subtitle: RectF,
    pub nav: Vec<RectF>,
    pub nav_width: f32,
    pub view: RectF,
    pub groups: Vec<GroupBox>,
    pub content_height: f32,
    pub scroll: f32,
    pub max_scroll: f32,
    pub scrollbar: Option<Thumb>,
    pub popup: Option<SelectPopup>,
}

/// Text width callback: (text, size, weight) -> pixel width.
pub type Measure<'a> = &'a dyn Fn(&str, f32, u16) -> f32;

pub const TITLE_SIZE: f32 = 13.5;
pub const DETAIL_SIZE: f32 = 11.5;
pub const BUTTON_SIZE: f32 = 12.5;

pub fn layout(width: i32, height: i32, dpi: u32, sections: &[Section], section: usize, scroll: f32, measure: Measure<'_>) -> Layout {
    let s = |value: f32| scale(value, dpi);
    let width = width.max(s(DIALOG_W.max(480.0)) as i32).max(1);
    let height = height.max(1);
    let header = RectF::new(0.0, 0.0, width as f32, s(HEADER_H));
    let close = RectF::new(width as f32 - s(16.0) - s(40.0), s(11.0), s(40.0), s(32.0));
    let title = RectF::new(s(20.0), 0.0, (width as f32 - s(80.0)).max(0.0), s(HEADER_H));
    let subtitle = RectF::new(s(20.0), s(30.0), (width as f32 - s(80.0)).max(0.0), s(18.0));
    let nav_width = s(NAV_W).min((width as f32 * 0.42).max(s(120.0)));
    let mut nav = Vec::with_capacity(sections.len());
    let mut y = s(HEADER_H) + s(6.0);
    for _ in sections {
        nav.push(RectF::new(s(NAV_INSET), y, (nav_width - s(NAV_INSET) * 2.0).max(0.0), s(NAV_ITEM_H)));
        y += s(NAV_ITEM_H + 2.0);
    }
    let view = RectF::new(nav_width + s(8.0), s(HEADER_H), (width as f32 - nav_width - s(8.0) - s(16.0)).max(0.0),
        (height as f32 - s(HEADER_H) - s(8.0)).max(0.0));
    let mut groups = Vec::new();
    let mut cursor = s(TITLE_H);
    // Every rectangle below is absolute: the painter and the hit test use the
    // same values, so the scroll offset stays the only thing applied on top.
    let content_left = view.x + s(4.0);
    let content_width = (view.w - s(12.0)).max(1.0);
    if let Some(current) = sections.get(section) {
        for group in &current.groups {
            let mut title_rect = None;
            if group.title.is_some() {
                title_rect = Some(RectF::new(content_left, cursor, content_width, s(GROUP_TITLE_H)));
                cursor += s(GROUP_TITLE_H);
            }
            let card_top = cursor;
            let mut rows = Vec::with_capacity(group.rows.len());
            let mut row_y = card_top;
            for row in &group.rows {
                let row_h = s(row_height(row));
                let rect = RectF::new(content_left, row_y, content_width, row_h);
                let (control, options, label) = control_boxes(row, rect, s(ROW_PAD_X), measure, dpi);
                rows.push(RowBox { rect, control, options, label });
                row_y += row_h;
            }
            let card = RectF::new(content_left, card_top, content_width, (row_y - card_top).max(1.0));
            cursor = row_y + s(CARD_GAP);
            groups.push(GroupBox { rect: card, rows, title: title_rect });
        }
    }
    let content_height = (cursor - s(CARD_GAP) + s(12.0) - view.h).max(0.0);
    let max_scroll = content_height.max(0.0);
    let scroll = scroll.clamp(0.0, max_scroll);
    let scrollbar = (max_scroll > 0.5).then(|| {
        let track = RectF::new(view.right() - s(SCROLLBAR_W + 2.0), view.y + s(4.0), s(SCROLLBAR_W), (view.h - s(8.0)).max(1.0));
        let ratio = (view.h / (view.h + max_scroll)).clamp(0.08, 1.0);
        let thumb_h = (track.h * ratio).max(s(28.0)).min(track.h);
        let offset = if max_scroll > 0.0 { (scroll / max_scroll) * (track.h - thumb_h) } else { 0.0 };
        Thumb { track, thumb: RectF::new(track.x, track.y + offset, track.w, thumb_h) }
    });
    let popup = sections.get(section).and_then(|current| {
        select_popup(current, &groups, scroll, view, height as f32, dpi)
    });
    Layout { width, height, dpi, header, close, title, subtitle, nav, nav_width, view, groups, content_height,
        scroll, max_scroll, scrollbar, popup }
}

/// Geometry of the expanded drop-down, if the section has one open. The list
/// drops below its box and flips above it when the window has no room left.
fn select_popup(current: &Section, groups: &[GroupBox], scroll: f32, view: RectF, height: f32, dpi: u32) -> Option<SelectPopup> {
    let s = |value: f32| scale(value, dpi);
    for (group_index, group) in groups.iter().enumerate() {
        let Some(rows) = current.groups.get(group_index).map(|group| &group.rows) else { continue };
        for (row_index, box_) in group.rows.iter().enumerate() {
            let Some(row) = rows.get(row_index) else { continue };
            if !matches!(row.control, Control::Select { open: true, .. }) || row.options.is_empty() { continue; }
            let control = box_.control;
            let base_y = view.y - scroll + control.y;
            let item_h = s(32.0);
            let pad = s(4.0);
            let list_h = row.options.len() as f32 * item_h + pad * 2.0;
            let mut y = base_y + control.h + s(2.0);
            if y + list_h > height - s(6.0) {
                y = (base_y - s(2.0) - list_h).max(s(HEADER_H) + s(40.0));
            }
            let rect = RectF::new(control.x, y, control.w, list_h);
            let options = (0..row.options.len())
                .map(|index| RectF::new(rect.x + pad, rect.y + pad + index as f32 * item_h, rect.w - pad * 2.0, item_h))
                .collect();
            return Some(SelectPopup { owner: (group_index, row_index), rect, options });
        }
    }
    None
}

fn row_height(row: &Row) -> f32 {
    if row.detail.is_empty() { 52.0 } else { 66.0 }
}

fn control_boxes(row: &Row, rect: RectF, pad: f32, measure: Measure<'_>, dpi: u32) -> (RectF, Vec<RectF>, Option<RectF>) {
    let s = |value: f32| scale(value, dpi);
    let right = rect.right() - pad;
    match &row.control {
        Control::Toggle { .. } => {
            let (w, h) = (s(44.0), s(22.0));
            (RectF::new(right - w, rect.y + (rect.h - h) / 2.0, w, h), Vec::new(), None)
        }
        Control::Segments => {
            let h = s(30.0);
            let widths: Vec<f32> = row.options.iter()
                .map(|option| (measure(option, s(12.0), 400) + s(24.0)).max(s(72.0)))
                .collect();
            let total: f32 = widths.iter().sum::<f32>() + s(4.0);
            let max = (rect.w - pad * 2.0 - s(220.0)).max(s(160.0));
            let scale_down = if total > max { max / total } else { 1.0 };
            let mut x = right - total * scale_down;
            let mut options = Vec::with_capacity(widths.len());
            for width in widths {
                options.push(RectF::new(x, rect.y + (rect.h - h) / 2.0 + s(1.0), width * scale_down - s(2.0), h - s(2.0)));
                x += width * scale_down;
            }
            (RectF::new(right - total * scale_down, rect.y + (rect.h - h) / 2.0, total * scale_down, h), options, None)
        }
        Control::Select { .. } => {
            let h = s(30.0);
            // Wide enough for the longest option plus the chevron column.
            let text = row.options.iter().map(|option| measure(option, s(12.5), 400)).fold(0.0f32, f32::max);
            let max = (rect.w - pad * 2.0 - s(200.0)).max(s(150.0));
            let w = (text + s(12.0) * 2.0 + s(40.0)).max(s(168.0)).min(max);
            (RectF::new(right - w, rect.y + (rect.h - h) / 2.0, w, h), Vec::new(), None)
        }
        Control::Button { .. } => {
            let h = s(30.0);
            let w = (measure(&row.label, s(BUTTON_SIZE), 400) + s(34.0)).max(s(76.0));
            (RectF::new(right - w, rect.y + (rect.h - h) / 2.0, w, h), Vec::new(), None)
        }
        Control::Value => {
            let h = rect.h;
            let w = (measure(&row.label, s(12.0), 400) + s(8.0)).max(s(40.0)).min((rect.w * 0.5).max(s(40.0)));
            (RectF::new(right - w, rect.y, w, h), Vec::new(), None)
        }
    }
}

impl Layout {
    /// Pointer position to the widget under it.
    pub fn hit(&self, x: f32, y: f32) -> Target {
        // The expanded list floats over everything, so it is tested first.
        if let Some(popup) = &self.popup {
            if let Some(option) = popup.options.iter().position(|rect| rect.contains(x, y)) {
                return Target::Option { group: popup.owner.0, row: popup.owner.1, option };
            }
        }
        if self.close.inset(-6.0, -6.0).contains(x, y) { return Target::Close; }
        if let Some(index) = self.nav.iter().position(|rect| rect.contains(x, y)) { return Target::Section(index); }
        if let Some(thumb) = self.scrollbar {
            if thumb.thumb.inset(-4.0, 0.0).contains(x, y) || thumb.track.inset(-4.0, 0.0).contains(x, y) {
                return Target::Scrollbar;
            }
        }
        if !self.view.contains(x, y) { return Target::None; }
        let local_y = y - self.view.y + self.scroll;
        for (group_index, group) in self.groups.iter().enumerate() {
            for (row_index, row) in group.rows.iter().enumerate() {
                if !row.rect.contains(x, local_y) { continue; }
                if let Some(option) = row.options.iter().position(|rect| rect.contains(x, local_y)) {
                    return Target::Option { group: group_index, row: row_index, option };
                }
                if row.control.contains(x, local_y) || row.rect.contains(x, local_y) {
                    return Target::Row { group: group_index, row: row_index };
                }
            }
        }
        Target::None
    }

    /// Scroll offset that reveals a row fully.
    pub fn reveal(&self, group: usize, row: usize) -> f32 {
        let Some(box_) = self.groups.get(group).and_then(|group| group.rows.get(row)) else { return self.scroll };
        let top = box_.rect.y - self.scroll;
        if top < 6.0 { return (box_.rect.y - 6.0).max(0.0); }
        let bottom = top + box_.rect.h;
        if bottom > self.view.h - 6.0 { return (self.scroll + bottom - self.view.h + 6.0).min(self.max_scroll); }
        self.scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure(text: &str, size: f32, _weight: u16) -> f32 {
        text.chars().map(|c| if c.is_ascii() { size * 0.58 } else { size }).sum()
    }
    fn snapshot() -> ExplorerUi { ExplorerUi::default() }
    fn sections_for(ui: &ExplorerUi) -> Vec<Section> {
        build(&Snapshot { ui, integration: 2, themed: true, open: None })
    }

    #[test]
    fn every_row_has_an_action_and_unique_id() {
        let ui = snapshot();
        let sections = sections_for(&ui);
        let mut ids = std::collections::HashSet::new();
        for section in &sections {
            for group in &section.groups {
                for row in &group.rows {
                    assert!(ids.insert(row.id), "duplicate row id {}", row.id);
                    match row.control {
                        Control::Value => assert!(row.actions.is_empty()),
                        Control::Segments | Control::Select { .. } => {
                            assert_eq!(row.actions.len(), row.options.len(), "{} needs one action per option", row.id);
                            assert!(row.options.len() >= 2);
                            assert!(row.selected < row.options.len(), "{} selects a missing option", row.id);
                        }
                        Control::Toggle { .. } | Control::Button { .. } => assert_eq!(row.actions.len(), 1, "{}", row.id),
                    }
                }
            }
        }
    }

    #[test]
    fn single_tab_build_has_no_tab_placement_rows_and_hides_unthemed_appearance() {
        let mut theme_snapshot = Snapshot { ui: &ExplorerUi::default(), integration: 2, themed: false, open: None };
        let plain = build(&theme_snapshot);
        assert!(!plain.iter().any(|section| section.id == "appearance"));
        theme_snapshot.themed = true;
        let themed = build(&theme_snapshot);
        assert!(themed.iter().any(|section| section.id == "appearance"));
        assert!(!themed.iter().flat_map(|section| &section.groups).flat_map(|group| &group.rows)
            .any(|row| row.id.contains("tab-placement")), "single-tab windows have no tab bar to place");
    }

    #[test]
    fn open_mode_defaults_to_a_new_window_and_follows_the_preference() {
        let mut ui = ExplorerUi::default();
        let row = |ui: &ExplorerUi| {
            build(&Snapshot { ui, integration: 2, themed: false, open: None })
                .into_iter().flat_map(|section| section.groups).flat_map(|group| group.rows)
                .find(|row| row.id == "open-mode").expect("open mode row")
        };
        let default = row(&ui);
        assert!(matches!(default.control, Control::Select { open: false, enabled: true }));
        assert_eq!(default.selected, 0);
        assert_eq!(default.action(None), Some("open-mode-window"));
        assert_eq!(default.action(Some(1)), Some("open-mode-tab"));
        ui.open_mode = OpenMode::NewTab;
        assert_eq!(row(&ui).selected, 1);
        assert_eq!(row(&ui).action(None), Some("open-mode-tab"));
    }

    #[test]
    fn an_open_select_floats_its_option_list_and_captures_the_pointer() {
        let ui = ExplorerUi::default();
        let sections = build(&Snapshot { ui: &ui, integration: 2, themed: false, open: Some("open-mode") });
        let index = sections.iter().position(|section| section.id == "general").unwrap();
        let closed = layout(940, 660, 96, &build(&Snapshot { ui: &ui, integration: 2, themed: false, open: None }), index, 0.0, &measure);
        assert!(closed.popup.is_none());
        let l = layout(940, 660, 96, &sections, index, 0.0, &measure);
        let popup = l.popup.as_ref().expect("open drop-down");
        assert_eq!(popup.owner, (0, 0));
        assert_eq!(popup.options.len(), 2);
        // The list hangs below its box, one row per option.
        assert!(popup.rect.y >= l.view.y + l.groups[0].rows[0].control.y);
        assert!(popup.options[1].y > popup.options[0].y);
        let option = &popup.options[1];
        assert_eq!(l.hit(option.x + 4.0, option.y + option.h / 2.0), Target::Option { group: 0, row: 0, option: 1 });
    }

    #[test]
    fn integration_rows_lock_when_takeover_is_off() {
        let ui = ExplorerUi::default();
        let off = build(&Snapshot { ui: &ui, integration: 0, themed: false, open: None });
        let rows: Vec<&Row> = off.iter().flat_map(|section| &section.groups).flat_map(|group| &group.rows).collect();
        let recycle = rows.iter().find(|row| row.id == "integration-recycle").expect("recycle row");
        assert!(matches!(recycle.control, Control::Toggle { enabled: false, .. }));
        let restore = rows.iter().find(|row| row.id == "integration-restore").expect("restore row");
        assert!(matches!(restore.control, Control::Button { enabled: false }));
        let on = build(&Snapshot { ui: &ui, integration: 2, themed: false, open: None });
        let rows: Vec<&Row> = on.iter().flat_map(|section| &section.groups).flat_map(|group| &group.rows).collect();
        assert!(matches!(rows.iter().find(|row| row.id == "integration-recycle").unwrap().control, Control::Toggle { enabled: true, .. }));
    }

    #[test]
    fn layout_stacks_cards_and_hit_tests_each_control() {
        let ui = snapshot();
        let sections = sections_for(&ui);
        let index = sections.iter().position(|section| section.id == "interface").unwrap();
        let l = layout(940, 660, 96, &sections, index, 0.0, &measure);
        assert!(l.groups.len() == 2, "界面 keeps the layout and preview cards; search scope lives in 搜索");
        // Cards never overlap and stay inside the content column.
        for pair in l.groups.windows(2) { assert!(pair[0].rect.bottom() <= pair[1].rect.y); }
        for group in &l.groups { assert!(group.rect.right() <= l.view.right()); }
        let group = &l.groups[0];
        let toggle = &group.rows[0];
        // Row rectangles are in content coordinates; the view scrolls them into place.
        let target = l.hit(toggle.control.x + 4.0, l.view.y + toggle.control.y + toggle.control.h / 2.0);
        assert_eq!(target, Target::Row { group: 0, row: 0 });
        let segments = &l.groups[1].rows[2];
        let option = &segments.options[1];
        assert_eq!(l.hit(option.x + 2.0, l.view.y + option.y + option.h / 2.0), Target::Option { group: 1, row: 2, option: 1 });
        assert_eq!(l.hit(l.close.x + 2.0, l.close.y + 2.0), Target::Close);
        assert_eq!(l.hit(l.nav[0].x + 2.0, l.nav[0].y + 2.0), Target::Section(0));
        assert_eq!(l.hit(l.view.x + 2.0, l.view.y + 2.0), Target::None);
    }

    #[test]
    fn cards_and_controls_stay_inside_the_content_column() {
        let ui = snapshot();
        let sections = sections_for(&ui);
        for (index, section) in sections.iter().enumerate() {
            let l = layout(940, 660, 96, &sections, index, 0.0, &measure);
            for group in &l.groups {
                // The card starts after the navigation column, not at the window edge.
                assert!(group.rect.x >= l.view.x, "{} card starts left of the content column", section.id);
                assert!(group.rect.right() <= l.view.right(), "{} card overflows the content column", section.id);
                for row in &group.rows {
                    assert!(row.rect.x >= group.rect.x && row.rect.right() <= group.rect.right() + 0.01);
                    assert!(row.control.right() <= row.rect.right() + 0.01, "{} control overflows its row", section.id);
                    assert!(row.control.x >= row.rect.x);
                    for option in &row.options {
                        assert!(option.x >= row.control.x - 0.01 && option.right() <= row.control.right() + 0.01);
                    }
                }
            }
        }
    }

    #[test]
    fn long_sections_scroll_and_reveal_rows() {
        let ui = snapshot();
        let sections = sections_for(&ui);
        let index = sections.iter().position(|section| section.id == "context").unwrap();
        let l = layout(940, 520, 96, &sections, index, 0.0, &measure);
        assert!(l.max_scroll > 0.0);
        assert!(l.scrollbar.is_some());
        let last = l.groups.last().unwrap();
        let last_row = last.rows.last().unwrap();
        let scroll = l.reveal(l.groups.len() - 1, last.rows.len() - 1);
        assert!(scroll > 0.0);
        let moved = layout(940, 520, 96, &sections, index, scroll, &measure);
        let row = moved.groups.last().unwrap().rows.last().unwrap();
        assert!(row.rect.y - moved.scroll + row.rect.h <= moved.view.h + 1.0);
        assert!(last_row.rect.y > 0.0);
    }

    #[test]
    fn scroll_is_clamped_to_the_content_height() {
        let ui = snapshot();
        let sections = sections_for(&ui);
        let index = sections.iter().position(|section| section.id == "general").unwrap();
        let l = layout(940, 900, 96, &sections, index, 5000.0, &measure);
        assert_eq!(l.max_scroll, 0.0);
        assert_eq!(l.scroll, 0.0);
        assert!(l.scrollbar.is_none());
    }

    #[test]
    fn high_dpi_scales_the_whole_layout() {
        let ui = snapshot();
        let sections = sections_for(&ui);
        let one = layout(940, 660, 96, &sections, 1, 0.0, &measure);
        let two = layout(1880, 1320, 192, &sections, 1, 0.0, &measure);
        assert!((two.nav_width - one.nav_width * 2.0).abs() < 2.0);
        assert!((two.groups[0].rows[0].control.h - one.groups[0].rows[0].control.h * 2.0).abs() < 2.0);
    }
}
