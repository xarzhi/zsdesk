//! Folder listing for the self-drawn file view.
//!
//! The Shell still enumerates the namespaces that only it understands — This
//! PC, the Recycle Bin, network places — but a plain filesystem folder does not
//! need it: one directory scan returns name, size, timestamp and attributes for
//! every entry. That is the difference between a window that hangs on a large
//! folder and one that opens it at once (measured on System32, 4940 entries:
//! 714 ms through the Shell, 7 ms through the scan).

use std::path::{Path, PathBuf};

use windows::{
    core::{PCWSTR, Result},
    Win32::{
        Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_SYSTEM,
        },
        System::{SystemServices::{SFGAO_FOLDER, SFGAO_HIDDEN}, WindowsProgramming::GetPrivateProfileStringW},
        UI::Shell::{
            BHID_EnumItems, IEnumShellItems, IShellItem, SHLoadIndirectString,
            SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY,
        },
    },
};
use zsdesk_core::model::explorer::ShellLocation;

use super::{icons, shell_item};

/// One row of the file view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEntry {
    /// Name shown in the list (folders may carry a `desktop.ini` rename).
    pub name: String,
    /// Absolute parsing name, used to rebuild the Shell item for menus and verbs.
    pub parsing_name: String,
    /// Filesystem path when the item has one (virtual items do not).
    pub path: Option<PathBuf>,
    pub is_dir: bool,
    pub hidden: bool,
    pub size: Option<u64>,
    /// Last write time in Unix milliseconds.
    pub modified: Option<u64>,
    /// Index in the system image list.
    pub icon: i32,
}

impl FolderEntry {
    /// Lower-case extension without the dot, for the 类型 column and sorting.
    pub fn kind(&self) -> String {
        if self.is_dir { return "文件夹".into(); }
        match self.path.as_ref().and_then(|path| path.extension()) {
            Some(extension) => { let text = extension.to_string_lossy().to_uppercase(); format!("{text} 文件") }
            None => "文件".into(),
        }
    }

    pub fn extension(&self) -> String {
        self.path.as_ref().and_then(|path| path.extension())
            .map(|extension| extension.to_string_lossy().to_lowercase()).unwrap_or_default()
    }

    /// Real file name, without the localized rename a folder may give itself.
    /// Commands hand this to the Shell, which parses it without guessing.
    pub fn file_name(&self) -> String {
        self.path.as_ref().and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.name.clone())
    }
}

/// Lists the children of `location`.
///
/// # Safety
///
/// COM must be initialized on the calling thread.
pub unsafe fn list(location: &ShellLocation) -> Result<Vec<FolderEntry>> {
    if let ShellLocation::Path(path) = location {
        // A namespace folder reached through a path (a control panel page, a
        // device) has no directory to scan, so the Shell takes it from here.
        if let Some(entries) = list_directory(path) { return Ok(entries); }
    }
    unsafe { list_shell(location) }
}

unsafe fn list_shell(location: &ShellLocation) -> Result<Vec<FolderEntry>> {
    let folder = unsafe { shell_item::shell_item(location) }?;
    let enumerator: IEnumShellItems = unsafe { folder.BindToHandler(None, &BHID_EnumItems) }?;
    let mut entries = Vec::new();
    loop {
        let mut fetched = [None];
        if unsafe { enumerator.Next(&mut fetched, None) }.is_err() { break; }
        let Some(item) = fetched[0].take() else { break };
        entries.push(describe(&item));
    }
    Ok(entries)
}

/// A directory scan reports times as 100 ns ticks since 1601; the list keeps
/// Unix milliseconds, like the Shell property it replaces.
const TICKS_PER_MS: u64 = 10_000;
const EPOCH_DIFFERENCE_MS: u64 = 11_644_473_600_000;

fn list_directory(path: &Path) -> Option<Vec<FolderEntry>> {
    use std::os::windows::fs::MetadataExt;
    let reader = std::fs::read_dir(path).ok()?;
    let mut entries = Vec::with_capacity(reader.size_hint().0);
    let mut names = Names::default();
    for item in reader.flatten() {
        let Ok(metadata) = item.metadata() else { continue };
        let attributes = metadata.file_attributes();
        // The Shell enumerates without SHCONTF_INCLUDEHIDDEN, which is what
        // Explorer shows by default: profile files and junctions stay hidden.
        if attributes & FILE_ATTRIBUTE_HIDDEN.0 != 0 { continue; }
        let file_name = item.file_name().to_string_lossy().into_owned();
        if file_name.is_empty() { continue; }
        let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
        let path = item.path();
        let name = if is_dir { names.folder_name(&path, &file_name, attributes) } else { names.file_name(&file_name) };
        let modified = (metadata.last_write_time() / TICKS_PER_MS).checked_sub(EPOCH_DIFFERENCE_MS);
        let icon = names.icon(&file_name, is_dir);
        let parsing_name = path.to_string_lossy().into_owned();
        entries.push(FolderEntry { name, parsing_name, path: Some(path), is_dir, hidden: false,
            size: (!is_dir).then(|| metadata.len()), modified, icon });
    }
    Some(entries)
}

/// Per-listing caches: one system-image-list lookup and one "is this a known
/// type" question per distinct extension instead of per file.
#[derive(Default)]
struct Names {
    hide_extensions: Option<bool>,
    icons: std::collections::HashMap<(String, bool), i32>,
    registered: std::collections::HashMap<String, bool>,
}

impl Names {
    /// `desktop.ini` rename, which is how Explorer (and the navigation tree)
    /// shows "桌面" for `C:\Users\me\Desktop`.
    fn folder_name(&mut self, path: &Path, file_name: &str, attributes: u32) -> String {
        localized_name(path, attributes).unwrap_or_else(|| file_name.to_string())
    }

    /// Explorer's rule for the name column: a shortcut never shows ".lnk", a
    /// file with a registered type hides its extension while "隐藏已知文件类型
    /// 的扩展名" is on, and an unknown extension is always shown.
    fn file_name(&mut self, file_name: &str) -> String {
        let Some((stem, extension)) = file_name.rsplit_once('.') else { return file_name.to_string() };
        if stem.is_empty() { return file_name.to_string(); }
        if extension.eq_ignore_ascii_case("lnk") { return stem.to_string(); }
        if !self.hide_extensions() || !self.registered_type(extension) { return file_name.to_string(); }
        stem.to_string()
    }

    fn hide_extensions(&mut self) -> bool {
        *self.hide_extensions.get_or_insert_with(hide_known_extensions)
    }

    fn registered_type(&mut self, extension: &str) -> bool {
        let key = extension.to_ascii_lowercase();
        if let Some(known) = self.registered.get(&key) { return *known; }
        let known = is_registered_type(&key);
        self.registered.insert(key, known);
        known
    }

    fn icon(&mut self, name: &str, is_dir: bool) -> i32 {
        let key = (Path::new(name).extension().map(|extension| extension.to_string_lossy().to_lowercase())
            .unwrap_or_default(), is_dir);
        if let Some(index) = self.icons.get(&key) { return *index; }
        let index = icons::icon_index_for_name(name, is_dir).unwrap_or(0);
        self.icons.insert(key, index);
        index
    }
}

/// Reads the name a folder gives itself in `desktop.ini`.
///
/// The Shell marks such a folder read-only or system to protect the file, so
/// the ini is only looked for there; a plain folder costs no extra file open.
fn localized_name(directory: &Path, attributes: u32) -> Option<String> {
    if attributes & (FILE_ATTRIBUTE_READONLY.0 | FILE_ATTRIBUTE_SYSTEM.0) == 0 { return None; }
    let ini = wide(&directory.join("desktop.ini").to_string_lossy());
    let section = wide(".ShellClassInfo");
    let key = wide("LocalizedResourceName");
    let mut buffer = [0u16; 256];
    let length = unsafe {
        GetPrivateProfileStringW(PCWSTR(section.as_ptr()), PCWSTR(key.as_ptr()), PCWSTR::null(),
            Some(&mut buffer), PCWSTR(ini.as_ptr()))
    } as usize;
    let value = String::from_utf16_lossy(&buffer[..length.min(buffer.len())]);
    let value = value.trim();
    if value.is_empty() { return None; }
    // "@%SystemRoot%\system32\shell32.dll,-21770" names a resource, not text.
    if !value.starts_with('@') { return Some(value.to_string()); }
    let source = wide(value);
    let mut text = [0u16; 256];
    if unsafe { SHLoadIndirectString(PCWSTR(source.as_ptr()), &mut text, None) }.is_err() { return None; }
    let text = String::from_utf16_lossy(&text);
    let text = text.trim_end_matches('\0').trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// "隐藏已知文件类型的扩展名" in 文件夹选项. Explorer hides them by default, and
/// the list keeps showing what Explorer shows.
fn hide_known_extensions() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut value = 1u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe { RegGetValueW(HKEY_CURRENT_USER,
        PCWSTR(wide(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced").as_ptr()),
        PCWSTR(wide("HideFileExt").as_ptr()), RRF_RT_REG_DWORD, None,
        Some((&mut value as *mut u32).cast()), Some(&mut size)) };
    status.0 == 0 && value != 0
}

/// True when Windows has a registered type for `extension` (lower-case, no dot).
fn is_registered_type(extension: &str) -> bool {
    use windows::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY_CLASSES_ROOT, KEY_READ};
    let path = wide(&format!(".{extension}"));
    let mut key = Default::default();
    if unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, PCWSTR(path.as_ptr()), Some(0), KEY_READ, &mut key) }.0 != 0 {
        return false;
    }
    let _ = unsafe { RegCloseKey(key) };
    true
}

fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(Some(0)).collect() }

fn describe(item: &IShellItem) -> FolderEntry {
    let attributes = unsafe { item.GetAttributes(SFGAO_FOLDER | SFGAO_HIDDEN) }.map(|value| value.0).unwrap_or(0);
    let name = unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) }.map(|value| unsafe { super::take(value) }).unwrap_or_default();
    // Recycle Bin items report their original path as the display name; the row
    // shows the file name, like Explorer's "Name" column does.
    let name = match name.rsplit_once(['\\', '/']) {
        Some((_, tail)) if !tail.trim().is_empty() => tail.to_string(),
        _ => name,
    };
    let path = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok()
        .map(|value| PathBuf::from(unsafe { super::take(value) }))
        .filter(|path| !path.as_os_str().is_empty());
    let parsing_name = unsafe { item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING) }
        .map(|value| unsafe { super::take(value) }).unwrap_or_default();
    let is_dir = attributes & SFGAO_FOLDER.0 != 0;
    let hidden = attributes & SFGAO_HIDDEN.0 != 0;
    let (size, modified) = match path.as_ref().and_then(|path| std::fs::metadata(path).ok()) {
        Some(metadata) => {
            let size = (!is_dir).then(|| metadata.len());
            let modified = metadata.modified().ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_millis() as u64);
            (size, modified)
        }
        None => (None, None),
    };
    let icon = icons::icon_index_for_name(&name, is_dir).unwrap_or(0);
    FolderEntry { name, parsing_name, path, is_dir, hidden, size, modified, icon }
}

/// Rebuilds the Shell item of an entry, for context menus and verbs.
pub fn item_of(entry: &FolderEntry) -> Option<IShellItem> {
    super::pidl::item_from_pidl(&pidl_of(entry)?).ok()
}

/// Absolute PIDL bytes of an entry.
pub fn pidl_of(entry: &FolderEntry) -> Option<Vec<u8>> {
    let item = super::pidl::item_from_parsing_name(&entry.parsing_name).ok()?;
    super::pidl::item_pidl(&item)
}

/// System image-list index of a location's icon.
pub fn icon_index_of(location: &ShellLocation) -> i32 {
    let index = unsafe { shell_item::shell_item(location) }.ok()
        .and_then(|item| super::pidl::item_pidl(&item))
        .and_then(|pidl| icons::icon_index(&pidl));
    index.or_else(|| icons::icon_index_for_name(&location.title(), true)).unwrap_or(0)
}

/// Top-level rows of the self-drawn navigation tree: label, target, icon index.
pub fn tree_roots() -> Vec<(String, ShellLocation, i32)> {
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_NetworkFolder,
        FOLDERID_Pictures, FOLDERID_Videos,
    };
    let mut roots: Vec<(String, ShellLocation)> = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from).filter(|path| path.is_dir()) {
        roots.push(("主文件夹".into(), ShellLocation::Path(home)));
    }
    for (folder, title) in [
        (&FOLDERID_Desktop, "桌面"),
        (&FOLDERID_Downloads, "下载"),
        (&FOLDERID_Documents, "文档"),
        (&FOLDERID_Pictures, "图片"),
        (&FOLDERID_Music, "音乐"),
        (&FOLDERID_Videos, "视频"),
    ] {
        if let Some(path) = crate::desktop::known_folder(folder) {
            roots.push((title.into(), ShellLocation::Path(path)));
        }
    }
    roots.push(("此电脑".into(), ShellLocation::ThisPc));
    if let Some(path) = crate::desktop::known_folder(&FOLDERID_NetworkFolder) {
        roots.push(("网络".into(), ShellLocation::Path(path)));
    }
    roots.push(("回收站".into(), ShellLocation::recycle_bin()));
    roots.into_iter().map(|(name, location)| { let icon = icon_index_of(&location); (name, location, icon) }).collect()
}

/// Sub-folders of `location`, the children of one navigation-tree row.
pub fn folder_children(location: &ShellLocation) -> Vec<FolderEntry> {
    let mut items = unsafe { list(location) }.unwrap_or_default();
    items.retain(|item| item.is_dir && !item.hidden);
    items.sort_by_key(|item| item.name.to_lowercase());
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_and_extension_describe_files_and_folders() {
        let file = FolderEntry { name: "report.PDF".into(), parsing_name: String::new(), path: Some(PathBuf::from(r"C:\a\report.PDF")),
            is_dir: false, hidden: false, size: Some(1), modified: None, icon: 0 };
        assert_eq!(file.extension(), "pdf");
        assert_eq!(file.kind(), "PDF 文件");
        let folder = FolderEntry { name: "docs".into(), parsing_name: String::new(), path: Some(PathBuf::from(r"C:\a\docs")),
            is_dir: true, hidden: false, size: None, modified: None, icon: 0 };
        assert_eq!(folder.kind(), "文件夹");
        assert_eq!(folder.extension(), "");
    }

    #[test]
    fn a_file_is_known_by_its_real_name_not_the_localized_folder_name() {
        let folder = FolderEntry { name: "桌面".into(), parsing_name: r"C:\Users\me\Desktop".into(),
            path: Some(PathBuf::from(r"C:\Users\me\Desktop")), is_dir: true, hidden: false,
            size: None, modified: None, icon: 0 };
        assert_eq!(folder.file_name(), "Desktop");
    }

    #[test]
    fn the_name_column_follows_the_explorer_extension_setting() {
        let mut names = Names { hide_extensions: Some(true), ..Default::default() };
        // A shortcut never shows its extension, a known type follows the
        // setting, and an unknown one is always spelled out.
        names.registered.insert("txt".into(), true);
        assert_eq!(names.file_name("报告.txt"), "报告");
        assert_eq!(names.file_name("快捷方式.lnk"), "快捷方式");
        assert_eq!(names.file_name("没有扩展名"), "没有扩展名");
        assert_eq!(names.file_name(".gitignore"), ".gitignore");
        assert_eq!(names.file_name("笔记.不是已知类型"), "笔记.不是已知类型");
        names.hide_extensions = Some(false);
        assert_eq!(names.file_name("报告.txt"), "报告.txt");
        assert_eq!(names.file_name("快捷方式.lnk"), "快捷方式");
    }

    #[test]
    fn the_current_system_lists_a_real_directory_with_sizes_and_dates() {
        let directory = std::env::current_dir().expect("a working directory");
        let entries = list_directory(&directory).expect("the crate directory is a real folder");
        assert!(!entries.is_empty());
        let manifest = entries.iter().find(|entry| entry.parsing_name.ends_with("Cargo.toml"))
            .expect("the manifest is listed");
        assert!(!manifest.is_dir);
        assert!(manifest.size.is_some_and(|size| size > 0), "a file has its size");
        assert!(manifest.modified.is_some(), "a file has its last write time");
        assert!(manifest.path.is_some() && manifest.icon != 0, "a row knows where it lives and which icon it uses");
        assert!(entries.iter().all(|entry| !entry.hidden), "hidden entries stay out of the list");
    }
}
