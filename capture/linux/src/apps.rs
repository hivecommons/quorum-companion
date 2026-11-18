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

/// The JSON array `list` prints, in table order: the meeting apps in `running`, each with
/// whether it is `in_call` (the keys are what the companion app reads).
pub fn list_json(
    running: &BTreeSet<&'static str>,
    in_call: &std::collections::BTreeMap<String, String>,
) -> serde_json::Value {
    let list: Vec<serde_json::Value> = MEETING_APPS
        .iter()
        .filter(|m| running.contains(m.id))
        .map(|m| {
            serde_json::json!({
                "bundle_id": m.id,
                "app": m.app,
                "name": m.name,
                "in_call": in_call.contains_key(m.id),
            })
        })
        .collect();
    serde_json::Value::Array(list)
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

    #[test]
    fn list_json_is_an_array_with_the_keys_the_app_reads() {
        let empty = std::collections::BTreeMap::new();
        assert_eq!(list_json(&BTreeSet::new(), &empty), serde_json::json!([]));
        let running: BTreeSet<&'static str> = ["slack", "zoom"].into_iter().collect();
        let in_call = std::collections::BTreeMap::from([("zoom".to_string(), "zoom".to_string())]);
        let value = list_json(&running, &in_call);
        let entries = value.as_array().expect("list is a JSON array");
        // Table order (zoom before slack), not the set's order.
        let ids: Vec<&str> = entries
            .iter()
            .map(|e| e["bundle_id"].as_str().expect("bundle_id is a string"))
            .collect();
        assert_eq!(ids, ["zoom", "slack"]);
        for entry in entries {
            let mut keys: Vec<&str> = entry
                .as_object()
                .expect("an entry is an object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ["app", "bundle_id", "in_call", "name"]);
        }
        assert_eq!(entries[0]["in_call"], true);
        assert_eq!(entries[1]["in_call"], false);
    }
}
