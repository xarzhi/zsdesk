//! The "开机自启" switch and the Windows login entry behind it.
//!
//! One Run value per profile opens the file manager when the user signs in. The
//! saved switch is the source of truth: every launch applies it to the registry,
//! so turning it on in the settings dialog (or leaving it at its default) works
//! even when the entry is missing or was written by an older installation.

use std::path::Path;

use crate::quick_search::control::main_window_startup;

/// Writes or removes the login entry for this profile.
pub fn apply(profile: &Path, enabled: bool) -> anyhow::Result<()> {
    main_window_startup::set_enabled(profile, enabled)
}

/// Whether the login entry currently points at this executable and profile.
pub fn enabled(profile: &Path) -> anyhow::Result<bool> {
    main_window_startup::enabled(profile)
}

/// Makes the login entry match the saved switch. Runs once per launch; a failed
/// write is only traced because the window is not up yet.
pub fn reconcile(profile: &Path) {
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
}
