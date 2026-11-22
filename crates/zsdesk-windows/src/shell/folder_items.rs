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
        System::{SystemServices::{SFGAO_FOLDER, SFGAO_HIDDEN, SFGAO_STREAM}, WindowsProgramming::GetPrivateProfileStringW},
        UI::Shell::{
            BHID_EnumItems, IEnumShellItems, IShellItem, SHGetKnownFolderItem, SHLoadIndirectString,
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Profile,
            FOLDERID_Pictures, FOLDERID_Videos, KF_FLAG_DEFAULT,
            SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY,
        },
    },
};
use zsdesk_core::model::explorer::ShellLocation;

use super::{drives::Drive, icons, shell_item};

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
    /// Creation time in Unix milliseconds, for the 创建日期 column.
    pub created: Option<u64>,
    /// Index in the system image list.
    pub icon: i32,
    /// Volume behind the entry, when it is a drive root. 此电脑 draws these as
    /// device cards with a capacity bar instead of plain rows.
    pub drive: Option<Drive>,
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
    ///
    /// A drive root has no last segment to report, and the Shell's 此电脑
    /// folder parses it by its letter, so `D:\` is the name commands need —
    /// "本地磁盘 (D:)" is a display name no verb accepts.
    pub fn file_name(&self) -> String {
        if let Some(drive) = &self.drive { return format!("{}:\\", drive.letter); }
        self.path.as_ref().and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.name.clone())
    }
}

/// Lists the children of `location`.
///
/// `include_hidden` follows 查看 → 隐藏的项目: the Shell hides them unless the
/// user asked for them, and the drawn list has to obey the same switch.
///
/// # Safety
///
/// COM must be initialized on the calling thread.
pub unsafe fn list(location: &ShellLocation, include_hidden: bool) -> Result<Vec<FolderEntry>> {
    if let ShellLocation::Path(path) = location {
        // A namespace folder reached through a path (a control panel page, a
        // device) has no directory to scan, so the Shell takes it from here.
        if let Some(entries) = list_directory(path, include_hidden) { return Ok(entries); }
    }
    let entries = unsafe { list_shell(location) }?;
    // 此电脑 lists devices, not files: Explorer shows a drive that is present
    // but not ready (an empty card reader, an open tray) instead of hiding it,
    // and the hidden attribute the Shell puts on those means nothing here.
    let mut entries: Vec<FolderEntry> = if include_hidden || matches!(location, ShellLocation::ThisPc) {
        entries
    } else {
        entries.into_iter().filter(|entry| !entry.hidden).collect()
    };
    // 此电脑 is not a folder of files: it is the drive list, and Explorer puts
    // it in letter order. The Shell's own enumeration depends on how the
    // devices were mounted, so a USB disk can land between C: and D:.
    if matches!(location, ShellLocation::ThisPc) { sort_drives(&mut entries); }
    Ok(entries)
}

/// Drive roots by letter, the way 此电脑 lists them: C:, D:, E: … and then
/// anything the Shell calls an item without a volume.
fn sort_drives(entries: &mut [FolderEntry]) {
    entries.sort_by(|a, b| match (&a.drive, &b.drive) {
        (Some(a), Some(b)) => a.letter.cmp(&b.letter),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => zsdesk_core::model::view::natural_cmp(&a.name, &b.name),
    });
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

fn list_directory(path: &Path, include_hidden: bool) -> Option<Vec<FolderEntry>> {
    use std::os::windows::fs::MetadataExt;
    let reader = std::fs::read_dir(path).ok()?;
    let mut entries = Vec::with_capacity(reader.size_hint().0);
    let mut names = Names::default();
    for item in reader.flatten() {
        let Ok(metadata) = item.metadata() else { continue };
        let attributes = metadata.file_attributes();
        // The Shell enumerates without SHCONTF_INCLUDEHIDDEN, which is what
        // Explorer shows by default: profile files and junctions stay hidden.
        let hidden = attributes & FILE_ATTRIBUTE_HIDDEN.0 != 0;
        if hidden && !include_hidden { continue; }
        let file_name = item.file_name().to_string_lossy().into_owned();
        if file_name.is_empty() { continue; }
        let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
        let path = item.path();
        let name = if is_dir { names.folder_name(&path, &file_name, attributes) } else { names.file_name(&file_name) };
        let modified = (metadata.last_write_time() / TICKS_PER_MS).checked_sub(EPOCH_DIFFERENCE_MS);
        let created = (metadata.creation_time() / TICKS_PER_MS).checked_sub(EPOCH_DIFFERENCE_MS);
        let icon = names.icon(&file_name, is_dir);
        let parsing_name = path.to_string_lossy().into_owned();
        entries.push(FolderEntry { name, parsing_name, path: Some(path), is_dir, hidden,
            size: (!is_dir).then(|| metadata.len()), modified, created, icon, drive: None });
    }
    Some(entries)
}

/// Rows for a set of paths, the way a search result page needs them.
///
/// The index answers with paths only, while the drawn list needs the same
/// name, size, timestamp and icon a directory scan produces. Paths that
/// vanished between the index read and this call are skipped, exactly like the
/// Shell's own results folder does when an item cannot be materialized.
///
/// # Safety
///
/// COM must be initialized on the calling thread.
pub unsafe fn entries_for_paths(paths: &[PathBuf], include_hidden: bool) -> Vec<FolderEntry> {
    use std::os::windows::fs::MetadataExt;
    let mut names = Names::default();
    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        let Ok(metadata) = std::fs::metadata(path) else { continue };
        let attributes = metadata.file_attributes();
        let hidden = attributes & FILE_ATTRIBUTE_HIDDEN.0 != 0;
        if hidden && !include_hidden { continue; }
        let Some(file_name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else { continue };
        if file_name.is_empty() { continue; }
        let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
        let name = if is_dir { names.folder_name(path, &file_name, attributes) } else { names.file_name(&file_name) };
        let modified = (metadata.last_write_time() / TICKS_PER_MS).checked_sub(EPOCH_DIFFERENCE_MS);
        let created = (metadata.creation_time() / TICKS_PER_MS).checked_sub(EPOCH_DIFFERENCE_MS);
        let icon = names.icon(&file_name, is_dir);
        entries.push(FolderEntry { name, parsing_name: path.to_string_lossy().into_owned(), path: Some(path.clone()),
            is_dir, hidden, size: (!is_dir).then(|| metadata.len()), modified, created, icon, drive: None });
    }
    entries
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
    let (size, modified, created) = match path.as_ref().and_then(|path| std::fs::metadata(path).ok()) {
        Some(metadata) => {
            let size = (!is_dir).then(|| metadata.len());
            let millis = |time: std::io::Result<std::time::SystemTime>| time.ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_millis() as u64);
            (size, millis(metadata.modified()), millis(metadata.created()))
        }
        None => (None, None, None),
    };
    let drive = path.as_ref().and_then(|path| Drive::of_root(path));
    // A drive root wears the volume icon (a hard disk, a USB stick, a disc),
    // not the folder icon a plain directory would get.
    let icon = match (&drive, path.as_ref()) {
        (Some(_), Some(path)) => icons::icon_index_for_path(path),
        _ => None,
    }.or_else(|| icons::icon_index_for_name(&name, is_dir)).unwrap_or(0);
    FolderEntry { name, parsing_name, path, is_dir, hidden, size, modified, created, icon, drive }
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

/// One row of the self-drawn navigation tree.
#[derive(Clone, Debug)]
pub struct TreeRow {
    /// Label shown next to the icon; empty on a separator.
    pub name: String,
    /// Folder the row opens; `None` marks a separator line.
    pub location: Option<ShellLocation>,
    /// System image-list index.
    pub icon: i32,
    /// Identity of the item inside the namespace it came from.
    ///
    /// A pinned quick-access folder is a child of the quick-access namespace,
    /// not a plain folder: its menu offers "从快速访问取消固定", while the same
    /// folder reached by path only offers "固定到快速访问". Keeping the PIDL is
    /// what lets the pane hand the Shell the item Explorer itself would.
    pub pidl: Option<Vec<u8>>,
}

/// Quick access: the folders the user pinned, in the order Explorer shows them.
pub const QUICK_ACCESS: &str = "shell:::{679F85CB-0220-4080-B29B-5540CC05AAB6}";
/// Gallery (图库), present from Windows 11 22H2 on.
const GALLERY: &str = "shell:::{e88865ea-0e1c-4e20-9aa6-edcd0212c87c}";
/// Network places. It has no filesystem path, so it is opened by CLSID.
const NETWORK: &str = "shell:::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}";

/// Top-level rows of the self-drawn navigation tree.
///
/// The layout follows Explorer's own pane: 主文件夹 and 图库 on their own, a
/// rule, the pinned quick-access folders, another rule, then 此电脑 and 网络.
/// The quick-access list is read from the Shell namespace rather than assumed,
/// so pinning or unpinning a folder in Explorer changes this pane too.
pub fn tree_roots() -> Vec<TreeRow> {
    let mut rows: Vec<TreeRow> = Vec::new();
    if let Some(home) = crate::desktop::known_folder(&FOLDERID_Profile) {
        let location = ShellLocation::Path(home);
        rows.push(TreeRow { name: "主文件夹".into(), icon: icon_index_of(&location), location: Some(location), pidl: None });
    }
    if let Some(gallery) = shell_row(GALLERY, "图库") { rows.push(gallery); }
    rows.push(separator());
    let mut quick_access = quick_access_rows();
    if quick_access.is_empty() {
        // A system without the quick-access namespace still gets the folders
        // Explorer pins by default, so the pane is never empty above 此电脑.
        for folder in [&FOLDERID_Desktop, &FOLDERID_Downloads, &FOLDERID_Documents,
            &FOLDERID_Pictures, &FOLDERID_Music, &FOLDERID_Videos] {
            if let Some(row) = known_folder_row(folder) { quick_access.push(row); }
        }
    }
    rows.extend(quick_access);
    rows.push(separator());
    rows.push(TreeRow { name: "此电脑".into(), icon: icon_index_of(&ShellLocation::ThisPc), location: Some(ShellLocation::ThisPc), pidl: None });
    if let Some(network) = shell_row(NETWORK, "网络") { rows.push(network); }
    rows
}

/// A blank line that divides the pane into groups, like Explorer's rules.
fn separator() -> TreeRow { TreeRow { name: String::new(), location: None, icon: 0, pidl: None } }

/// A row for one Shell namespace, resolved through its CLSID.
fn shell_row(parsing_name: &str, title: &str) -> Option<TreeRow> {
    let location = ShellLocation::Shell { parsing_name: parsing_name.into(), title: title.into() };
    let item = unsafe { shell_item::shell_item(&location) }.ok()?;
    Some(TreeRow { name: title.into(), icon: item_icon(&item), location: Some(location), pidl: None })
}

/// A row for a known folder: the name Explorer uses, its own icon, and the
/// filesystem path when the folder has one.
fn known_folder_row(folder: &windows::core::GUID) -> Option<TreeRow> {
    let item = unsafe { SHGetKnownFolderItem::<IShellItem>(folder, KF_FLAG_DEFAULT, None) }.ok()?;
    let name = unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) }.map(|value| unsafe { super::take(value) }).unwrap_or_default();
    let path = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok()
        .map(|value| PathBuf::from(unsafe { super::take(value) }))
        .filter(|path| !path.as_os_str().is_empty());
    let location = match path {
        Some(path) => ShellLocation::Path(path),
        // A folder with no directory behind it (a library, a device) is only
        // resolvable through its absolute parsing name; the display name is not
        // a parsing name and would leave the row pointing at nothing.
        None => {
            let parsing_name = unsafe { item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING) }
                .map(|value| unsafe { super::take(value) }).unwrap_or_default();
            if parsing_name.is_empty() { return None; }
            ShellLocation::Shell { parsing_name, title: name.clone() }
        }
    };
    Some(TreeRow { name, icon: item_icon(&item), location: Some(location), pidl: None })
}

/// The pinned quick-access folders, read from the Shell namespace that
/// Explorer itself fills. Recent files live in the same namespace but are not
/// folders, so they never reach the pane.
fn quick_access_rows() -> Vec<TreeRow> {
    // A pinned known folder keeps the icon and the localized name Explorer
    // shows for it; only the user's own pins fall back to their item name.
    let known = known_folder_lookup();
    let mut rows = Vec::new();
    for child in quick_access_folders() {
        let path = unsafe { child.GetDisplayName(SIGDN_FILESYSPATH) }.ok()
            .map(|value| PathBuf::from(unsafe { super::take(value) }))
            .filter(|path| !path.as_os_str().is_empty());
        let pinned = path.as_ref().map(|path| normalize(path)).and_then(|key| known.get(&key).cloned());
        let name = pinned.as_ref().map(|(name, _)| name.clone())
            .unwrap_or_else(|| unsafe { child.GetDisplayName(SIGDN_NORMALDISPLAY) }
                .map(|value| unsafe { super::take(value) }).unwrap_or_default());
        let icon = pinned.map(|(_, icon)| icon).unwrap_or_else(|| item_icon(&child));
        let location = match path {
            Some(path) => ShellLocation::Path(path),
            None => ShellLocation::Shell {
                parsing_name: unsafe { child.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING) }
                    .map(|value| unsafe { super::take(value) }).unwrap_or_default(),
                title: name.clone(),
            },
        };
        rows.push(TreeRow { name, icon, location: Some(location), pidl: super::pidl::item_pidl(&child) });
    }
    rows
}

/// Identity of the pinned quick-access group: the absolute parsing name of
/// every pinned folder, in Explorer's order.
///
/// Pinning or unpinning a folder — from this pane's menu, from a folder's menu
/// or from Explorer itself — changes this list. Comparing it is how the pane
/// notices the change without re-reading icons and names on every focus.
pub fn quick_access_signature() -> Vec<String> {
    quick_access_folders().into_iter()
        .map(|child| unsafe { child.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING) }
            .map(|value| unsafe { super::take(value) }).unwrap_or_default())
        .collect()
}

/// The folder items of the quick-access namespace, in Explorer's order.
///
/// Recent files live in the same namespace but are not folders, so they are
/// filtered out here instead of in every caller.
fn quick_access_folders() -> Vec<IShellItem> {
    let location = ShellLocation::Shell { parsing_name: QUICK_ACCESS.into(), title: "快速访问".into() };
    let Ok(folder) = (unsafe { shell_item::shell_item(&location) }) else { return Vec::new() };
    let Ok(items) = (unsafe { folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) }) else { return Vec::new() };
    let mut folders = Vec::new();
    loop {
        let mut fetched = [None];
        if unsafe { items.Next(&mut fetched, None) }.is_err() { break; }
        let Some(child) = fetched[0].take() else { break };
        let attributes = unsafe { child.GetAttributes(SFGAO_FOLDER | SFGAO_STREAM | SFGAO_HIDDEN) }
            .map(|value| value.0).unwrap_or(0);
        if attributes & SFGAO_FOLDER.0 == 0 || attributes & (SFGAO_STREAM.0 | SFGAO_HIDDEN.0) != 0 { continue; }
        folders.push(child);
    }
    folders
}

/// Names and icons of the six folders Explorer pins on a fresh profile.
fn known_folder_lookup() -> std::collections::HashMap<String, (String, i32)> {
    let mut lookup = std::collections::HashMap::new();
    for folder in [&FOLDERID_Desktop, &FOLDERID_Downloads, &FOLDERID_Documents,
        &FOLDERID_Pictures, &FOLDERID_Music, &FOLDERID_Videos] {
        let Some(path) = crate::desktop::known_folder(folder) else { continue };
        let Some(row) = known_folder_row(folder) else { continue };
        lookup.insert(normalize(&path), (row.name, row.icon));
    }
    lookup
}

/// Case-insensitive key for a folder path, so a Shell item and a known folder
/// compare equal.
fn normalize(path: &Path) -> String { path.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase() }

/// System image-list index of a Shell item, through its PIDL.
fn item_icon(item: &IShellItem) -> i32 {
    super::pidl::item_pidl(item).and_then(|pidl| icons::icon_index(&pidl)).unwrap_or(0)
}

/// Sub-folders of `location`, the children of one navigation-tree row.
pub fn folder_children(location: &ShellLocation) -> Vec<FolderEntry> {
    let mut items = unsafe { list(location, false) }.unwrap_or_default();
    // 此电脑 already arrives in drive order and carries the drives that are
    // present but not ready; a folder tree keeps only the folders it can open.
    if matches!(location, ShellLocation::ThisPc) {
        items.retain(|item| item.is_dir);
        return items;
    }
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
            is_dir: false, hidden: false, size: Some(1), modified: None, created: None, icon: 0, drive: None };
        assert_eq!(file.extension(), "pdf");
        assert_eq!(file.kind(), "PDF 文件");
        let folder = FolderEntry { name: "docs".into(), parsing_name: String::new(), path: Some(PathBuf::from(r"C:\a\docs")),
            is_dir: true, hidden: false, size: None, modified: None, created: None, icon: 0, drive: None };
        assert_eq!(folder.kind(), "文件夹");
        assert_eq!(folder.extension(), "");
    }

    #[test]
    fn a_file_is_known_by_its_real_name_not_the_localized_folder_name() {
        let folder = FolderEntry { name: "桌面".into(), parsing_name: r"C:\Users\me\Desktop".into(),
            path: Some(PathBuf::from(r"C:\Users\me\Desktop")), is_dir: true, hidden: false,
            size: None, modified: None, created: None, icon: 0, drive: None };
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
    fn drives_are_listed_by_letter_not_by_their_display_name() {
        let drive = |letter: char, name: &str| FolderEntry { name: name.into(), parsing_name: format!("{letter}:\\"),
            path: Some(PathBuf::from(format!("{letter}:\\"))), is_dir: true, hidden: false,
            size: None, modified: None, created: None, icon: 0,
            drive: Some(Drive { letter, kind: super::super::drives::DriveKind::Fixed, total: 1, free: 1 }) };
        // The Shell can hand the volumes over in any order, and sorting by the
        // localized name would put "Lenovo (G:)" above "本地磁盘 (C:)".
        let mut entries = vec![drive('G', "Lenovo (G:)"), drive('C', "本地磁盘 (C:)"), drive('E', "新加卷 (E:）")];
        sort_drives(&mut entries);
        assert_eq!(entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            ["本地磁盘 (C:)", "新加卷 (E:）", "Lenovo (G:)"]);
    }

    #[test]
    fn the_current_system_lists_a_real_directory_with_sizes_and_dates() {
        let directory = std::env::current_dir().expect("a working directory");
        let entries = list_directory(&directory, false).expect("the crate directory is a real folder");
        assert!(!entries.is_empty());
        let manifest = entries.iter().find(|entry| entry.parsing_name.ends_with("Cargo.toml"))
            .expect("the manifest is listed");
        assert!(!manifest.is_dir);
        assert!(manifest.size.is_some_and(|size| size > 0), "a file has its size");
        assert!(manifest.modified.is_some(), "a file has its last write time");
        assert!(manifest.created.is_some(), "a file has its creation time");
        assert!(manifest.path.is_some() && manifest.icon != 0, "a row knows where it lives and which icon it uses");
        assert!(entries.iter().all(|entry| !entry.hidden), "hidden entries stay out of the list");
    }
}
