//! Meeting apps the companion offers to listen to on Linux.
//!
//! On Linux the app id the companion passes around (`bundle_id` in the helper's JSON, the
//! argument of `capture`) is a short name of the meeting app's executable, e.g. `zoom`,
//! `teams-for-linux` or `chrome`. PipeWire tags every stream with the binary that opened it
//! (`application.process.binary`), so the same table maps both PipeWire nodes and processes.
//! Plain logic, tested on every platform.

use std::collections::BTreeSet;

/// A meeting app: its id, the name hive records, the name the window shows, and the
/// executables (lower case, without a path) that belong to it.
#[derive(Debug, PartialEq, Eq)]
pub struct MeetingApp {
    pub id: &'static str,
    pub app: &'static str,
    pub name: &'static str,
    pub binaries: &'static [&'static str],
}

const fn meeting_app(
    id: &'static str,
    app: &'static str,
    name: &'static str,
    binaries: &'static [&'static str],
) -> MeetingApp {
    MeetingApp {
        id,
        app,
        name,
        binaries,
    }
}

/// Executables are matched without regard to case or path. Browsers play and record from
/// their main binary on Linux (Chrome's audio service runs as `chrome`), so no helper-process
/// mapping is needed. The list order is the order `list` reports.
pub const MEETING_APPS: &[MeetingApp] = &[
    meeting_app(
        "zoom",
        "zoom",
        "Zoom",
        &["zoom", "zoom.real", "zoomwebviewhost"],
    ),
    meeting_app(
        "teams-for-linux",
        "teams",
        "Microsoft Teams",
        &["teams-for-linux", "teams", "teams-insiders"],
    ),
    meeting_app("slack", "slack", "Slack", &["slack"]),
    meeting_app("webex", "webex", "Webex", &["webex", "ciscocollabhost"]),
    meeting_app(
        "chrome",
        "browser",
        "Google Chrome",
        &["chrome", "google-chrome", "google-chrome-stable"],
    ),
    meeting_app(
        "chromium",
        "browser",
        "Chromium",
        &["chromium", "chromium-browser"],
    ),
    meeting_app(
        "msedge",
        "browser",
        "Microsoft Edge",
        &["msedge", "microsoft-edge"],
    ),
    meeting_app("brave", "browser", "Brave", &["brave", "brave-browser"]),
    meeting_app(
        "firefox",
        "browser",
        "Firefox",
        &["firefox", "firefox-bin", "firefox-esr"],
    ),
    meeting_app(
        "vivaldi",
        "browser",
        "Vivaldi",
        &["vivaldi", "vivaldi-bin", "vivaldi-stable"],
    ),
];

/// The executable name as the table has it: no directory, no " (deleted)" suffix the kernel
/// adds to `/proc/<pid>/exe` after an update, lower case.
fn normalize(binary: &str) -> String {
    let binary = binary.trim();
    let binary = binary.strip_suffix(" (deleted)").unwrap_or(binary);
    let name = binary.rsplit('/').next().unwrap_or(binary);
    name.to_ascii_lowercase()
}

/// The meeting app an executable (a name or a path) belongs to.
pub fn by_binary(binary: &str) -> Option<&'static MeetingApp> {
    let name = normalize(binary);
    MEETING_APPS
        .iter()
        .find(|m| m.binaries.contains(&name.as_str()))
}

/// The meeting app for an id the companion passed to `capture`.
pub fn by_id(id: &str) -> Option<&'static MeetingApp> {
    MEETING_APPS.iter().find(|m| m.id == id)
}

/// The ids of the meeting apps among running executables (process names or paths).
pub fn running<'a>(names: impl IntoIterator<Item = &'a str>) -> BTreeSet<&'static str> {
    names
        .into_iter()
        .filter_map(by_binary)
        .map(|m| m.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binaries_match_without_path_case_or_deleted_suffix() {
        assert_eq!(by_binary("zoom").map(|m| m.id), Some("zoom"));
        assert_eq!(by_binary("/opt/zoom/zoom").map(|m| m.id), Some("zoom"));
        assert_eq!(by_binary("ZoomWebviewHost").map(|m| m.app), Some("zoom"));
        let deleted = "/opt/google/chrome/chrome (deleted)";
        assert_eq!(by_binary(deleted).map(|m| m.id), Some("chrome"));
        assert_eq!(by_binary("teams-for-linux").map(|m| m.app), Some("teams"));
        assert_eq!(by_binary("CiscoCollabHost").map(|m| m.app), Some("webex"));
        assert_eq!(by_binary(" firefox-esr\n").map(|m| m.app), Some("browser"));
        assert_eq!(by_binary("pw-record"), None);
        assert_eq!(by_binary(""), None);
    }

    #[test]
    fn every_id_is_unique_and_is_one_of_its_own_binaries() {
        let ids: BTreeSet<&str> = MEETING_APPS.iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), MEETING_APPS.len());
        for m in MEETING_APPS {
            assert_eq!(by_id(m.id), Some(m));
            assert_eq!(by_binary(m.id), Some(m));
            assert!(m.binaries.iter().all(|b| normalize(b) == *b), "{}", m.id);
        }
        assert_eq!(by_id("skype"), None);
    }

    #[test]
    fn hive_app_names_match_the_macos_helper() {
        let names: BTreeSet<&str> = MEETING_APPS.iter().map(|m| m.app).collect();
        let expected: BTreeSet<&str> = ["browser", "slack", "teams", "webex", "zoom"]
            .into_iter()
            .collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn running_apps_come_from_process_names_and_paths() {
        let names = ["bash", "slack", "/usr/bin/chromium", "zoom.real"];
        let ids: Vec<&str> = running(names).into_iter().collect();
        assert_eq!(ids, ["chromium", "slack", "zoom"]);
        assert!(running(Vec::<&str>::new()).is_empty());
    }
}
