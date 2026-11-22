//! Drive letters: which root a path is, how full the volume is, and what kind
//! of disk sits behind it.
//!
//! The 此电脑 view draws one card per drive with a capacity bar, and the Shell
//! hands out only the name ("本地磁盘 (C:)"). The three numbers the bar needs —
//! total size, free space, drive type — come straight from the volume APIs, so
//! the card matches what Explorer shows to the byte.

use std::path::Path;

use windows::{core::PCWSTR, Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW}};

/// `GetDriveTypeW` return values. The constants live in a header the `windows`
/// crate splits differently per version, so the numbers are spelled out here.
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Ram,
    Unknown,
}

impl DriveKind {
    fn from_type(value: u32) -> Self {
        match value {
            DRIVE_FIXED => DriveKind::Fixed,
            DRIVE_REMOVABLE => DriveKind::Removable,
            DRIVE_REMOTE => DriveKind::Network,
            DRIVE_CDROM => DriveKind::Optical,
            DRIVE_RAMDISK => DriveKind::Ram,
            _ => DriveKind::Unknown,
        }
    }

    /// Word the card uses when a drive has no volume label of its own.
    pub fn label(self) -> &'static str {
        match self {
            DriveKind::Fixed => "本地磁盘",
            DriveKind::Removable => "可移动磁盘",
            DriveKind::Network => "网络驱动器",
            DriveKind::Optical => "DVD 驱动器",
            DriveKind::Ram => "RAM 磁盘",
            DriveKind::Unknown => "驱动器",
        }
    }
}

/// One mounted drive root, with the numbers its capacity bar is drawn from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drive {
    /// Upper-case drive letter, without the colon.
    pub letter: char,
    pub kind: DriveKind,
    /// Size of the volume in bytes; zero for a device that is not ready.
    pub total: u64,
    /// Free space in bytes.
    pub free: u64,
}

impl Drive {
    /// Reads the drive behind `path`, when `path` is a root such as `C:\`.
    ///
    /// A device that is not ready (an empty card reader, an open optical drive)
    /// still reports a letter and a type, so the card is drawn without a bar
    /// instead of disappearing from 此电脑.
    pub fn of_root(path: &Path) -> Option<Self> {
        let letter = drive_letter(path)?;
        let root = format!("{letter}:\\");
        let wide = wide(&root);
        let kind = DriveKind::from_type(unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) });
        if kind == DriveKind::Unknown { return None; }
        let mut free = 0u64;
        let mut total = 0u64;
        let mut available = 0u64;
        let measured = unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut available), Some(&mut total), Some(&mut free)) };
        let (total, free) = if measured.is_ok() { (total, available) } else { (0, 0) };
        Some(Self { letter, kind, total, free })
    }

    /// Bytes in use, which is what the coloured part of the bar shows.
    pub fn used(&self) -> u64 { self.total.saturating_sub(self.free) }

    /// Share of the volume in use, 0.0 … 1.0. Zero for an unready device.
    pub fn fill(&self) -> f32 {
        if self.total == 0 { return 0.0; }
        ((self.used() as f64 / self.total as f64) as f32).clamp(0.0, 1.0)
    }
}

/// Upper-case letter of a drive root: `C:\`, `C:/` and `C:` all answer `C`.
fn drive_letter(path: &Path) -> Option<char> {
    let text = path.as_os_str().to_str()?;
    let mut chars = text.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() { return None; }
    let rest = chars.as_str();
    if matches!(rest, "" | ":" | ":\\" | ":/") { Some(letter.to_ascii_uppercase()) } else { None }
}

fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(Some(0)).collect() }

/// Size in the three-significant-digit form Explorer uses: `64.6 GB`,
/// `465 GB`, `1.81 TB`. Digits are truncated, never rounded up, because that is
/// what Windows itself does — 1.8198 TB reads as 1.81 TB, not 1.82 TB.
pub fn short_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 { return format!("{bytes} B"); }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() { value /= 1024.0; unit += 1; }
    let decimals = if value < 10.0 { 2 } else if value < 100.0 { 1 } else { 0 };
    let scale = 10f64.powi(decimals as i32);
    let truncated = (value * scale).floor() / scale;
    format!("{truncated:.decimals$} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn only_a_drive_root_answers_with_a_letter() {
        assert_eq!(drive_letter(Path::new(r"C:\")), Some('C'));
        assert_eq!(drive_letter(Path::new("d:/")), Some('D'));
        assert_eq!(drive_letter(Path::new("E:")), Some('E'));
        assert_eq!(drive_letter(Path::new(r"C:\Windows")), None);
        assert_eq!(drive_letter(Path::new(r"\\server\share")), None);
        assert_eq!(drive_letter(Path::new("")), None);
    }

    #[test]
    fn the_card_sizes_read_like_explorer() {
        // The numbers from This PC on this machine: a 465 GB disk with 64.6 GB
        // free, and a 1.81 TB one. Truncation matters: rounding would show 466
        // GB and 1.82 TB.
        assert_eq!(short_size(500_363_685_888), "465 GB");
        assert_eq!(short_size(69_400_891_392), "64.6 GB");
        assert_eq!(short_size(2_000_381_014_016), "1.81 TB");
        assert_eq!(short_size(1_000_185_266_176), "931 GB");
        assert_eq!(short_size(512), "512 B");
        // Three significant digits: 1.536 KB reads as 1.50 KB, not 1.5 KB.
        assert_eq!(short_size(1_536), "1.50 KB");
        assert_eq!(short_size(0), "0 B");
    }

    #[test]
    fn a_root_of_a_mounted_volume_knows_its_letter_and_size() {
        let drive = Drive::of_root(&PathBuf::from(r"C:\")).expect("the system drive is mounted");
        assert_eq!(drive.letter, 'C');
        assert_eq!(drive.kind, DriveKind::Fixed);
        assert!(drive.total > 0 && drive.free <= drive.total);
        assert!((0.0..=1.0).contains(&drive.fill()));
        assert_eq!(drive.used(), drive.total - drive.free);
        assert_eq!(Drive::of_root(Path::new(r"C:\Windows")), None, "a folder is not a drive");
    }

    #[test]
    fn an_unready_device_keeps_its_card_without_a_bar() {
        // No letter is guaranteed to be mounted here; the arithmetic is what
        // matters: a zero-size volume never divides and never fills.
        let drive = Drive { letter: 'Q', kind: DriveKind::Optical, total: 0, free: 0 };
        assert_eq!(drive.fill(), 0.0);
        assert_eq!(drive.used(), 0);
    }
}
