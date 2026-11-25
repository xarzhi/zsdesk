//! ZSDesk application entry.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod commands;
mod edition;
mod storage;
mod window;
mod wallpaper;
mod settings;
mod instance;
mod reveal;
mod context_preferences;
mod routing_control;
mod fast_search;
mod quick_search;
mod startup;
mod win_e_bridge;
mod explorer_forwarder;
mod runtime;
mod context_menu_process;

/// Lets the installer wait for all application windows to finish before
/// replacing or uninstalling their files.
struct ApplicationLifetime(Option<windows::Win32::Foundation::HANDLE>);
impl ApplicationLifetime {
    fn new() -> Self {
        if app::is_import_cli() { return Self(None); }
        use windows::Win32::System::Threading::{CreateMutexExW, SYNCHRONIZATION_SYNCHRONIZE};
        let name: Vec<u16> = crate::edition::application_mutex().encode_utf16().chain(Some(0)).collect();
        Self(unsafe { CreateMutexExW(None, windows::core::PCWSTR(name.as_ptr()), 0, SYNCHRONIZATION_SYNCHRONIZE.0) }.ok())
    }
}
impl Drop for ApplicationLifetime {
    fn drop(&mut self) { if let Some(handle) = self.0.take() { let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) }; } }
}

/// Hides the console window a console-subsystem build is handed.
///
/// Release builds are GUI programs, but a debug build is compiled as a console
/// program so tracing reaches the terminal. Started by Explorer or by a login
/// entry, that console belongs to nobody: it just sits on the desktop as an
/// empty black window. A console that also serves a shell is left alone, so
/// running the program from a terminal keeps the terminal — only a console
/// created for this process alone (`GetConsoleProcessList` reporting one
/// process) is ours to hide.
fn hide_own_console() {
    use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    let mut attached = [0u32; 2];
    if unsafe { GetConsoleProcessList(&mut attached) } != 1 { return; }
    let console = unsafe { GetConsoleWindow() };
    if console.0.is_null() { return; }
    let _ = unsafe { ShowWindow(console, SW_HIDE) };
}

fn main() {
    // Command-line modes answer on stdout, so their console stays visible.
    if !app::is_import_cli() { hide_own_console(); }
    if let Some(result)=context_menu_process::run_worker_if_requested(){if let Err(error)=result{eprintln!("{error:#}");std::process::exit(1);}return;}
    if let Some(result)=window::preview_process::run_if_requested(){
        if let Err(error)=result{eprintln!("{error:#}");std::process::exit(1);}return;
    }
    let _lifetime = ApplicationLifetime::new();
    if let Err(error)=app::run(){
        if app::is_import_cli() || app::is_background_mode() {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        // Folders opened through system integration must never lose their opener.
        if let Some(target)=app::shell_open_target() {
            let _=app::open_original_explorer(target);
            return;
        }
        if app::is_recycle_bin() && app::open_original_explorer(zsdesk_core::model::explorer::RECYCLE_BIN_ADDRESS.into()).is_ok(){return;}
        if app::is_win_e() && app::open_original_explorer("shell:MyComputerFolder".into()).is_ok(){return;}
        let message:Vec<u16>=format!("{error:#}").encode_utf16().chain(Some(0)).collect();
        unsafe {windows::Win32::UI::WindowsAndMessaging::MessageBoxW(None,windows::core::PCWSTR(message.as_ptr()),
            windows::core::w!("ZSDesk"),windows::Win32::UI::WindowsAndMessaging::MB_OK|windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR);}
        std::process::exit(1);
    }
}
