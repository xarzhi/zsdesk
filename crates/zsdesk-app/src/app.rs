use std::{ffi::OsString, path::{Path, PathBuf}};
use anyhow::{bail, Context, Result};
use zsdesk_core::model::explorer::{ExplorerSession, ExplorerTab, ShellLocation};
use crate::{storage, window};

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Explorer,
    QuickSearch(crate::quick_search::Action),
}

impl Default for Mode { fn default() -> Self { Self::Explorer } }

#[derive(Default)]
struct Options {
    profile: Option<PathBuf>,
    classic: bool,
    target: Option<OsString>,
    select: bool,
    mode: Mode,
    diagnostics_main: Option<PathBuf>,
    uninstall_integration: bool,
    routing_apply: bool,
    integration_refresh: bool,
    integration_enable: bool,
    win_e: bool,
    new_window: bool,
    /// Screen point of the drop that asked for this window (tab drag-out).
    at: Option<(i32, i32)>,
    focus_search:bool,
    search_settings:bool,
}

fn parse(args: impl IntoIterator<Item=OsString>) -> Result<Options> {
    let mut opts = Options::default();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg=="--focus-search"{opts.focus_search=true;continue;}
        if arg=="--search-settings"{opts.focus_search=true;opts.search_settings=true;continue;}
        if arg=="--integration-enable"{opts.integration_enable=true;continue;}
        if arg == "--new-window" { opts.new_window=true; continue; }
        if arg == "--at" {
            opts.at = Some(parse_point(&iter.next().context("缺少窗口位置")?)?);
            continue;
        }
        let quick = match arg.to_str() {
            Some("--quick-search") => Some(crate::quick_search::Action::Open),
            Some("--quick-search-settings") => Some(crate::quick_search::Action::Settings),
            Some("--background") => Some(crate::quick_search::Action::Resident),
            Some("--background-stop") => Some(crate::quick_search::Action::Stop),
            Some("--background-status") => Some(crate::quick_search::Action::Status),
            Some("--background-enable") => Some(crate::quick_search::Action::EnableAtLogin),
            Some("--background-disable") => Some(crate::quick_search::Action::Disable),
            _ => None,
        };
        if let Some(action) = quick {
            if opts.mode != Mode::Explorer { bail!("快速搜索不能与其他启动模式混用"); }
            opts.mode = Mode::QuickSearch(action); continue;
        }
        if arg == "--uninstall-integration" { opts.uninstall_integration = true; continue; }
        if arg == "--integration-refresh" {opts.integration_refresh=true;continue;}
        if arg == "--win-e" {opts.win_e=true;continue;}
        if arg == "--routing-apply" { opts.routing_apply = true; continue; }
        if arg=="--diagnostics-main"{opts.diagnostics_main=Some(iter.next().context("缺少诊断报告路径")?.into());continue;}
        if arg == "--classic-menu" { opts.classic = true; continue; }
        if arg == "--profile-dir" { opts.profile = Some(PathBuf::from(iter.next().context("缺少配置目录")?)); continue; }
        if arg == "--select" { opts.select = true; continue; }
        // Registered by system integration: behaves like a plain path argument.
        if arg == "--shell-open" { continue; }
        let target = if arg == "--this-pc" { OsString::from("shell:MyComputerFolder") }
            else if arg == "--recycle-bin" { OsString::from(zsdesk_core::model::explorer::RECYCLE_BIN_ADDRESS) }
            else { repair_path_argument(arg) };
        if opts.target.replace(target).is_some() { bail!("只能指定一个文件或目录"); }
    }
    if opts.select && opts.target.is_none() { bail!("缺少定位目标"); }
    if opts.at.is_some() && opts.target.is_none() { bail!("窗口位置需要同时给出打开目标"); }
    if opts.integration_enable&&(opts.mode!=Mode::Explorer||opts.target.is_some()||opts.select||opts.classic||opts.integration_refresh||opts.uninstall_integration||opts.routing_apply||opts.diagnostics_main.is_some()||opts.new_window||opts.win_e){bail!("启用系统集成不能与其他操作混用");}
    if opts.win_e && (opts.mode!=Mode::Explorer || opts.target.is_some() || opts.select || opts.classic || opts.integration_refresh || opts.uninstall_integration || opts.routing_apply || opts.diagnostics_main.is_some()) {bail!("Win+E 启动不能与其他操作混用");}
    if opts.integration_refresh && (opts.mode!=Mode::Explorer || opts.target.is_some() || opts.select || opts.classic || opts.uninstall_integration || opts.routing_apply || opts.diagnostics_main.is_some()) {bail!("集成修复不能与其他操作混用");}
    if matches!(opts.mode, Mode::QuickSearch(_)) && (opts.target.is_some() || opts.select || opts.classic) { bail!("快速搜索启动不能附带文件浏览参数"); }
    if opts.uninstall_integration && (opts.mode != Mode::Explorer || opts.target.is_some() || opts.select || opts.classic || opts.diagnostics_main.is_some()) {
        bail!("卸载恢复不能与文件浏览或其他启动模式混用");
    }
    if opts.routing_apply && (opts.uninstall_integration || opts.mode != Mode::Explorer || opts.target.is_some() || opts.select || opts.classic || opts.diagnostics_main.is_some()) {
        bail!("后台文件位置设置不能与其他启动模式混用");
    }
    if opts.diagnostics_main.is_some() && (opts.mode!=Mode::Explorer || opts.target.is_some() || opts.select){bail!("窗口诊断不能与其他启动模式混用");}
    Ok(opts)
}

/// `--at 1200,540`: the screen point a dragged tab was dropped on.
fn parse_point(value: &OsString) -> Result<(i32, i32)> {
    let text = value.to_string_lossy();
    let (x, y) = text.split_once(',').context("窗口位置应写成 x,y")?;
    Ok((x.trim().parse().context("窗口位置横坐标无效")?, y.trim().parse().context("窗口位置纵坐标无效")?))
}

/// Target of a `--shell-open` launch, so `main` can fall back to Explorer.
pub fn shell_open_target() -> Option<OsString> {
    let mut args = std::env::args_os().skip_while(|a| a != "--shell-open");
    args.next()?;
    args.next().map(repair_path_argument)
}

/// The registered verbs quote the path as `--shell-open "%V"`. A drive root
/// ends with a backslash right before that closing quote, which the Windows
/// command line parser reads as an escaped quote: `C:\` arrives as `C:"`.
/// Quotes cannot occur in a path, so any trailing ones are leftovers.
fn repair_path_argument(argument: OsString) -> OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let units: Vec<u16> = argument.encode_wide().collect();
    let mut repaired = units.clone();
    while repaired.last() == Some(&u16::from(b'"')) { repaired.pop(); }
    // `C:` alone means "current directory on C:", not the root.
    if repaired.len() == 2 && repaired[1] == u16::from(b':') { repaired.push(u16::from(b'\\')); }
    if repaired == units { argument } else { OsString::from_wide(&repaired) }
}

/// Used before parsing succeeds so malformed background commands never show UI.
pub fn is_import_cli() -> bool {
    std::env::args_os().skip(1).any(|arg| arg == "--diagnostics-main" || arg == "--build-info" || arg == "--uninstall-integration" || arg == "--routing-apply"
        || arg == "--background-stop" || arg == "--background-status" || arg == "--background-enable" || arg == "--background-disable" || arg == "--integration-refresh" || arg=="--integration-enable")
}

pub fn is_background_mode() -> bool { std::env::args_os().any(|arg| arg == "--background") }
pub fn is_win_e() -> bool {std::env::args_os().skip(1).any(|arg|arg=="--win-e")}
/// Registered by the Recycle Bin verb: `main` falls back to Explorer.
pub fn is_recycle_bin() -> bool {std::env::args_os().skip(1).any(|arg|arg=="--recycle-bin")}

pub fn open_original_explorer(target:OsString)->std::io::Result<()> {
    use std::os::windows::ffi::OsStringExt;
    // The takeover would otherwise hide this window and forward it straight
    // back into ZSDesk, which is what made it fall back to Explorer.
    crate::explorer_forwarder::suppress(std::time::Duration::from_secs(4));
    let mut directory=vec![0u16;32768];
    let length=unsafe {windows::Win32::System::SystemInformation::GetWindowsDirectoryW(Some(&mut directory))} as usize;
    if length==0 || length>=directory.len(){return Err(std::io::Error::last_os_error());}
    let executable=PathBuf::from(OsString::from_wide(&directory[..length])).join("explorer.exe");
    std::process::Command::new(executable).arg(target).spawn().map(|_|())
}

pub fn run() -> Result<()> {
    run_arguments(std::env::args_os().skip(1).collect(),false)
}
/// Desktop Recycle Bin takeover is its own switch; a missing profile keeps the
/// documented default of "on".
fn recycle_bin_takeover(config:&Path)->bool{
    storage::load(config).map(|session|session.ui.recycle_bin_takeover).unwrap_or(true)
}

fn managed(options:&Options)->bool{
    !options.integration_enable&&!options.integration_refresh&&!options.routing_apply&&!options.uninstall_integration
        &&options.diagnostics_main.is_none()&&match &options.mode{
            Mode::Explorer=>true,
            Mode::QuickSearch(action)=>matches!(action,crate::quick_search::Action::Open|crate::quick_search::Action::Settings|crate::quick_search::Action::Resident),
        }
}
pub(crate) fn runtime_profile(args:&[OsString])->Result<Option<PathBuf>>{
    let options=parse(args.to_vec())?;
    if !managed(&options){return Ok(None);}
    Ok(Some(std::path::absolute(options.profile.map(|p|p.join("explorer-session.json")).unwrap_or_else(storage::config_path))?))
}
pub(crate) fn run_arguments(args:Vec<OsString>,in_host:bool)->Result<()> {
    if args.first().is_some_and(|arg| arg == "--build-info") {
        if args.len() != 1 { bail!("--build-info 不能与其他参数混用"); }
        println!("{}", serde_json::json!({"product":"ZSDesk", "version":env!("CARGO_PKG_VERSION"),
            "edition":crate::edition::key(), "edition_name":crate::edition::name(),
            "network":crate::edition::NETWORK, "themed":crate::edition::THEMED, "platform":"windows-x64"}));
        return Ok(());
    }
    let app_id = crate::edition::application_id();
    let app_id: Vec<u16> = app_id.encode_utf16().chain(Some(0)).collect();
    let _ = unsafe { windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(windows::core::PCWSTR(app_id.as_ptr())) };
    let opts = parse(args.iter().cloned())?;
    let config = std::path::absolute(opts.profile.as_ref().map(|p| p.join("explorer-session.json")).unwrap_or_else(storage::config_path))?;
    if !in_host&&managed(&opts){return crate::runtime::run(config,args);}
    let recycle = recycle_bin_takeover(&config);
    if opts.integration_enable{
        let exe=std::env::current_exe()?;
        zsdesk_windows::integration::apply(zsdesk_windows::integration::Integration::Default,&exe,&config.with_file_name("takeover-backup.json"),recycle)?;
        println!("{}",serde_json::json!({"enabled":true,"win_e":zsdesk_windows::integration::win_e_enabled(&exe),"this_pc":zsdesk_windows::integration::this_pc_enabled(&exe),"recycle_bin":zsdesk_windows::integration::recycle_bin_enabled(&exe)}));return Ok(());
    }
    if opts.integration_refresh {
        let changed=zsdesk_windows::integration::upgrade_default(&std::env::current_exe()?,&config.with_file_name("takeover-backup.json"),recycle)?;
        println!("{}",serde_json::json!({"updated":changed,"win_e":zsdesk_windows::integration::win_e_enabled(&std::env::current_exe()?),"recycle_bin":zsdesk_windows::integration::recycle_bin_enabled(&std::env::current_exe()?)}));
        return Ok(());
    }
    if opts.routing_apply {
        println!("{}", crate::routing_control::apply_owned_settings(&config)?);
        return Ok(());
    }
    if opts.uninstall_integration {
        let exe = std::env::current_exe()?;
        crate::quick_search::control::uninstall_owned(&config, &exe)?;
        crate::routing_control::uninstall_owned(&config, &exe)?;
        zsdesk_windows::integration::restore_owned(&exe, &config.with_file_name("takeover-backup.json"))?;
        return Ok(());
    }
    if let Some(report)=opts.diagnostics_main{return crate::instance::diagnose(&config,&std::path::absolute(report)?);}
    match opts.mode {
        Mode::QuickSearch(action) => return crate::quick_search::run(config, action),
        Mode::Explorer => {}
    }
    let mut requested = None;
    let mut selection = None;
    if let Some(argument) = opts.target {
        let text = argument.to_string_lossy();
        let location = if text.eq_ignore_ascii_case("shell:MyComputerFolder") {
            ShellLocation::ThisPc
        } else if text.starts_with("shell:") || text.starts_with("::{") {
            ShellLocation::Shell { parsing_name: text.into_owned(), title: "Shell".into() }
        } else {
            let path = std::path::absolute(PathBuf::from(&argument))?;
            if opts.select || path.is_file() {
                selection = Some(path.file_name().context("该项目没有父目录")?.to_owned());
                ShellLocation::Path(path.parent().context("该项目没有父目录")?.into())
            } else { ShellLocation::Path(path) }
        };
        requested = Some(location);
    }
    if opts.focus_search&&crate::instance::focus_search(&config,opts.search_settings)?{return Ok(());}
    // A session written by a newer build, or edited by hand, must not stop the
    // window from opening: fall back to the defaults instead of failing.
    let loaded = if config.try_exists()? {
        match storage::load(&config) {
            Ok(session) => Some(session),
            Err(error) => {
                crate::runtime::trace(&format!("session-load-failed {error:#}"));
                // Keep the unreadable file for the user instead of silently
                // overwriting it with the fresh session this launch will save.
                let _ = std::fs::rename(&config, config.with_extension("json.invalid"));
                None
            }
        }
    } else { None };
    // "Open in a new window" (the default) never reuses a window that already
    // shows something else. "Open as a tab" hands the folder to the window that
    // is already open, which adds a tab there.
    let open_mode = loaded.as_ref().map(|session| session.ui.open_mode)
        .unwrap_or(zsdesk_core::model::explorer::OpenMode::NewWindow);
    let (instance, additional) = if opts.new_window {
        (None, true)
    } else if open_mode.single_tab() {
        let (guard, first) = crate::instance::claim(&config)?;
        (Some(guard), !first)
    } else {
        match crate::instance::enter(&config, requested.as_ref(), selection.as_deref())? {
            Some(guard) => (Some(guard), false),
            None => return Ok(()),
        }
    };
    let _instance = instance;
    let fresh = loaded.is_none() || opts.new_window || additional;
    let mut session = loaded.unwrap_or_else(storage::default_session);
    session.normalize();
    if let Some(location) = requested { activate(&mut session, location, fresh); }
    window::run(session, config, selection, !opts.classic,opts.new_window||additional,opts.focus_search.then_some(opts.search_settings),opts.at)
}

fn activate(session: &mut ExplorerSession, target: ShellLocation, fresh: bool) {
    if session.ui.open_mode.single_tab() {
        // The requested location becomes the window's only tab; a tab that
        // already shows it keeps its saved view.
        let mut tab = match session.tabs.iter().position(|tab| tab.current() == &target) {
            Some(index) => session.tabs.remove(index),
            None => { let id = session.take_id(); ExplorerTab::new(id, target) }
        };
        tab.normalize();
        session.tabs.clear();
        session.tabs.push(tab);
        session.active_tab = 0;
        return;
    }
    if fresh { session.tabs.clear(); }
    if let Some(index) = session.tabs.iter().position(|t| t.current() == &target) {
        session.active_tab = index;
    } else {
        if fresh { session.tabs.clear(); }
        let id = session.take_id();
        session.tabs.push(ExplorerTab::new(id, target));
        session.active_tab = session.tabs.len() - 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_drive_root_survives_the_quoted_verb_argument() {
        // `--shell-open "%V"` for `C:\` reaches the parser as `C:"`.
        assert_eq!(repair_path_argument(r#"C:"#.into()), OsString::from(r"C:\"));
        assert_eq!(repair_path_argument(r"C:\Program Files".into()), OsString::from(r"C:\Program Files"));
        assert_eq!(repair_path_argument(r#"\\server\share\"#.into()), OsString::from(r#"\\server\share\"#));
        let options = parse(["--shell-open".into(), r#"D:"#.into()]).unwrap();
        assert_eq!(options.target, Some(OsString::from(r"D:\")));
    }
    #[test] fn win_e_starts_the_manager_without_a_synthetic_folder() {
        let options=parse(["--win-e"].map(OsString::from)).unwrap();
        assert!(options.win_e && options.target.is_none() && options.mode==Mode::Explorer);
        for args in [["--win-e","--background"],["--win-e","--integration-refresh"]] {assert!(parse(args.map(OsString::from)).is_err());}
    }
    #[test]
    fn reveal_replaces_the_only_tab_of_a_single_tab_window() {
        let mut s = ExplorerSession::default();
        let a = ShellLocation::Path("C:/a".into());
        let b = ShellLocation::Path("C:/b".into());
        activate(&mut s, a.clone(), true);
        activate(&mut s, b, false);
        assert_eq!(s.tabs.len(), 1, "a window never accumulates tabs");
        assert_eq!(s.active_tab, 0);
        assert_eq!(s.current_location(), &ShellLocation::Path("C:/b".into()));
        activate(&mut s, a.clone(), false);
        assert_eq!(s.tabs.len(), 1);
        assert_eq!(s.current_location(), &a);
    }
    #[test]
    fn recycle_bin_launch_opens_the_shell_folder() {
        let options=parse(["--recycle-bin"].map(OsString::from)).unwrap();
        assert_eq!(options.target.as_deref().map(|value|value.to_string_lossy().into_owned()).as_deref(),
            Some(zsdesk_core::model::explorer::RECYCLE_BIN_ADDRESS));
        assert!(!options.new_window&&!options.select);
    }
    #[test]
    fn selection_requires_target_and_keeps_unicode_names() {
        assert!(parse([OsString::from("--select")]).is_err());
        let opts = parse(["--select", "--classic-menu", r"C:\工作 文件\图纸.pdf"].map(OsString::from)).unwrap();
        assert!(opts.select && opts.classic);
        assert_eq!(opts.target.unwrap(), r"C:\工作 文件\图纸.pdf");
    }
    #[test]
    fn unknown_switches_stay_plain_paths() {
        // Switches that used to belong to the desktop fences are ordinary paths now.
        assert!(parse(["--organize"].map(OsString::from)).unwrap().target.is_some());
    }
}
