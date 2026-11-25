//! The "开机自启" switch and the Windows login entry behind it.
//!
//! One Run value per profile opens the file manager when the user signs in. The
//! saved switch is the source of truth: every launch applies it to the registry,
//! so turning it on in the settings dialog (or leaving it at its default) works
//! even when the entry is missing or was written by an older installation.

use std::path::Path;

use crate::quick_search::control::main_window_startup;

/// Writes or removes the login entry for this profile.
///
/// Removing is always allowed — a stale entry is exactly what has to go — but a
/// run that must not own an entry (`may_register`) leaves the registry alone
/// when asked to create one.
pub fn apply(profile: &Path, enabled: bool) -> anyhow::Result<()> {
    // Validated first, so a bad profile is still an error rather than a silent
    // no-op when this run may not register itself.
    anyhow::ensure!(profile.is_absolute(), "登录启动需要绝对的配置路径");
    if enabled && !may_register(profile) {
        crate::runtime::trace("autostart skipped: this run may not own a login entry");
        return Ok(());
    }
    main_window_startup::set_enabled(profile, enabled)
}

/// Whether the login entry currently points at this executable and profile.
pub fn enabled(profile: &Path) -> anyhow::Result<bool> {
    main_window_startup::enabled(profile)
}

/// Whether a run may own a login entry at all.
///
/// The entry outlives the run that wrote it, so three kinds of run must never
/// reach the registry:
///
/// - a debug build is compiled as a console program, and at login Windows parks
///   its empty console on the desktop as a black terminal window;
/// - a binary under a `target` directory is a build output, not an install, and
///   it disappears with the next `cargo clean`;
/// - a profile inside the temporary directory belongs to a test.
pub(crate) fn may_register(profile: &Path) -> bool {
    let executable = std::env::current_exe().unwrap_or_default();
    registration_allowed(cfg!(debug_assertions), &executable, profile)
}

/// The rule above, with its inputs explicit so it can be tested.
fn registration_allowed(debug: bool, executable: &Path, profile: &Path) -> bool {
    if debug { return false; }
    let build_output = executable.components().any(|part| part.as_os_str().eq_ignore_ascii_case("target"));
    let profile_directory = profile.parent().unwrap_or(profile);
    !build_output && !inside(profile_directory, &std::env::temp_dir())
}

/// Whether `path` is `root` or lives under it, ignoring separator style and case.
fn inside(path: &Path, root: &Path) -> bool {
    let flatten = |value: &Path| value.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    let (path, root) = (flatten(path), flatten(root));
    let root = root.trim_end_matches('\\');
    if root.is_empty() { return false; }
    path == root || path.starts_with(&format!("{root}\\"))
}

/// Makes the login entry match the saved switch. Runs once per launch; a failed
/// write is only traced because the window is not up yet.
pub fn reconcile(profile: &Path) {
    // A run that must not own an entry strips the one an earlier run of the same
    // binary left behind: that is how a debug build's console window, once
    // registered, would otherwise come back at every login.
    if !may_register(profile) {
        if enabled(profile).unwrap_or(false) && apply(profile, false).is_err() {
            crate::runtime::trace("autostart cleanup failed: this run may not own a login entry");
        }
        return;
    }
    // A profile without a session file keeps the documented default of "on".
    let wanted = crate::storage::load(profile).map(|session| session.ui.autostart).unwrap_or(true);
    match enabled(profile) {
        Ok(current) if current == wanted => {}
        Ok(_) => {
            if let Err(error) = apply(profile, wanted) {
                crate::runtime::trace(&format!("autostart apply failed wanted={wanted} error={error:#}"));
            }
        }
        Err(error) => crate::runtime::trace(&format!("autostart status failed error={error:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_profile_leaves_the_entry_alone() {
        // The reconcile path reads the session first; a missing file must not
        // panic and must keep the "on" default in mind.
        let missing = Path::new(r"C:\Profiles\Missing\explorer-session.json");
        assert!(crate::storage::load(missing).is_err());
        // `apply` validates absolute paths, which is what keeps a bad profile
        // from writing a login entry that runs the wrong thing.
        assert!(apply(Path::new("explorer-session.json"), true).is_err());
    }

    /// A login entry outlives the run that wrote it, so a build that would show
    /// a console window at sign-in — or a build that will not exist tomorrow —
    /// must never create one.
    #[test]
    fn only_an_installed_release_build_from_a_real_profile_owns_a_login_entry() {
        let profile = Path::new(r"C:\Users\me\AppData\Roaming\ZSDesk\explorer-session.json");
        let installed = Path::new(r"C:\Program Files\ZSDesk\ZSDesk.exe");
        assert!(registration_allowed(false, installed, profile));
        assert!(!registration_allowed(true, installed, profile), "a debug build is a console program");
        assert!(!registration_allowed(false, Path::new(r"C:\src\zsdesk\target\release\zsdesk-app.exe"), profile),
            "a build output goes away with the next cargo clean");
        let test_profile = std::env::temp_dir().join("zsdesk-check").join("explorer-session.json");
        assert!(!registration_allowed(false, installed, &test_profile), "a test profile lives in the temporary directory");
    }

    #[test]
    fn a_directory_test_ignores_separators_and_case_but_not_siblings() {
        assert!(super::inside(Path::new(r"C:\Temp\zsdesk\profile"), Path::new(r"C:\Temp")));
        assert!(super::inside(Path::new(r"c:/temp/profile"), Path::new(r"C:\Temp")));
        assert!(super::inside(Path::new(r"C:\Temp"), Path::new(r"C:\Temp")));
        assert!(!super::inside(Path::new(r"C:\Temp2\profile"), Path::new(r"C:\Temp")), "a sibling is not inside");
        assert!(!super::inside(Path::new(r"C:\Profiles\Temp"), Path::new(r"C:\Temp")));
    }
}
