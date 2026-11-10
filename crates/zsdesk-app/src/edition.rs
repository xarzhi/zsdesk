//! Build edition, selected with Cargo features (see scripts/package.ps1).
//!
//! | 版本 | features |
//! |---|---|
//! | 标准版 | （无） |
//! | 网络版 | `network`：FTP、SFTP/SSH、SMB 地址与网络驱动器 |
//! | 全量版 | `network`（默认） |
//! | 主题版 | `network` + `themed`：可选的窗口材质与颜色模式 |

pub const NETWORK: bool = cfg!(feature = "network");
pub const THEMED: bool = cfg!(feature = "themed");

pub const fn profile_name() -> &'static str { if THEMED { "ZSDesk-Theme" } else { "ZSDesk" } }
pub const fn application_id() -> &'static str { if THEMED { "ZSDesk.Theme" } else { "ZSDesk.Native" } }
pub const fn application_mutex() -> &'static str {
    if THEMED { "Local\\ZSDesk.Theme.Application" } else { "Local\\ZSDesk.Native.Application" }
}

pub const fn key() -> &'static str {
    if THEMED { return "themed"; }
    if NETWORK { "full" } else { "standard" }
}

pub const fn name() -> &'static str {
    if THEMED { return "主题版"; }
    if NETWORK { "全量版" } else { "标准版" }
}
