//! Meeting apps the companion offers to listen to on Windows, and which process tree to capture.
//!
//! On Windows the app id the companion passes around (`bundle_id` in the helper's JSON, the
//! argument of `capture`) is the meeting app's executable name in lower case, e.g. `zoom.exe`.
//! This module is plain logic over a process snapshot so it is tested on every platform.

use std::collections::{BTreeMap, HashMap, HashSet};

/// A meeting app: its executable, the name hive records, and the name the window shows.
#[derive(Debug, PartialEq, Eq)]
pub struct MeetingApp {
    pub exe: &'static str,
    pub app: &'static str,
    pub name: &'static str,
}

const fn meeting_app(exe: &'static str, app: &'static str, name: &'static str) -> MeetingApp {
    MeetingApp { exe, app, name }
}

/// Executables are matched without regard to case. Helper processes (a browser's renderers,
/// new Teams' WebView2 children) are found through their parent, see [`meeting_app_of`].
pub const MEETING_APPS: &[MeetingApp] = &[
    meeting_app("zoom.exe", "zoom", "Zoom"),
    meeting_app("ms-teams.exe", "teams", "Microsoft Teams"),
    meeting_app("teams.exe", "teams", "Microsoft Teams (classic)"),
    meeting_app("slack.exe", "slack", "Slack"),
    meeting_app("ciscocollabhost.exe", "webex", "Webex"),
    meeting_app("webex.exe", "webex", "Webex"),
    meeting_app("atmgr.exe", "webex", "Webex Meetings"),
    meeting_app("chrome.exe", "browser", "Google Chrome"),
    meeting_app("msedge.exe", "browser", "Microsoft Edge"),
    meeting_app("firefox.exe", "browser", "Firefox"),
    meeting_app("brave.exe", "browser", "Brave"),
    meeting_app("arc.exe", "browser", "Arc"),
];

/// How deep a parent chain is followed; parent ids can be stale or reused, so never loop.
const MAX_DEPTH: usize = 64;

/// One entry of a process snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub exe: String,
}

/// The meeting app for an executable name, ignoring case.
pub fn app_for_exe(exe: &str) -> Option<&'static MeetingApp> {
    MEETING_APPS.iter().find(|m| m.exe.eq_ignore_ascii_case(exe))
}

fn by_pid(procs: &[Process]) -> HashMap<u32, &Process> {
    procs.iter().map(|p| (p.pid, p)).collect()
}

/// The meeting app a process belongs to: the process itself or its nearest ancestor that is a
/// meeting app (Chrome's renderer belongs to chrome.exe, a WebView2 child to ms-teams.exe, and
/// Zoom started from a browser link is still Zoom).
pub fn meeting_app_of(pid: u32, procs: &[Process]) -> Option<&'static MeetingApp> {
    let index = by_pid(procs);
    let mut seen = HashSet::new();
    let mut current = index.get(&pid).copied();
    while let Some(p) = current {
        if !seen.insert(p.pid) || seen.len() > MAX_DEPTH {
            return None;
        }
        if let Some(app) = app_for_exe(&p.exe) {
            return Some(app);
        }
        if p.parent == p.pid {
            return None;
        }
        current = index.get(&p.parent).copied();
    }
    None
}

/// Meeting apps that have an active microphone session, keyed by executable.
pub fn apps_in_call(
    session_pids: &[u32],
    procs: &[Process],
) -> BTreeMap<&'static str, &'static str> {
    session_pids
        .iter()
        .filter_map(|pid| meeting_app_of(*pid, procs))
        .map(|m| (m.exe, m.app))
        .collect()
}

/// Meeting apps that are running, one entry per executable, in table order.
pub fn running_apps(procs: &[Process]) -> Vec<&'static MeetingApp> {
    MEETING_APPS
        .iter()
        .filter(|m| procs.iter().any(|p| m.exe.eq_ignore_ascii_case(&p.exe)))
        .collect()
}

/// Whether `pid` is `root` or one of its descendants.
pub fn in_tree(root: u32, pid: u32, procs: &[Process]) -> bool {
    let index = by_pid(procs);
    let mut current = pid;
    for _ in 0..MAX_DEPTH {
        if current == root {
            return true;
        }
        match index.get(&current) {
            Some(p) if p.parent != p.pid => current = p.parent,
            _ => return false,
        }
    }
    false
}

/// The process to hand WASAPI process loopback (with the "include process tree" mode) for an
/// executable: a top-level process of that executable, i.e. one whose parent is not the same
/// executable. When the app runs several top-level instances, the one whose tree holds an
/// active microphone session (`prefer`) wins, otherwise the oldest-looking (lowest) pid.
pub fn capture_root(exe: &str, procs: &[Process], prefer: &[u32]) -> Option<u32> {
    let index = by_pid(procs);
    let mut roots: Vec<u32> = procs
        .iter()
        .filter(|p| p.exe.eq_ignore_ascii_case(exe) && is_root(p, &index))
        .map(|p| p.pid)
        .collect();
    roots.sort_unstable();
    let preferred = roots
        .iter()
        .find(|root| prefer.iter().any(|pid| in_tree(**root, *pid, procs)));
    preferred.or(roots.first()).copied()
}

/// Whether a process is the top of its executable's tree (its parent is another executable).
fn is_root(p: &Process, index: &HashMap<u32, &Process>) -> bool {
    match index.get(&p.parent) {
        Some(parent) => parent.pid == p.pid || !parent.exe.eq_ignore_ascii_case(&p.exe),
        None => true,
    }
}

/// A change the `detect` mode reports.
#[derive(Debug, PartialEq, Eq)]
pub enum CallEvent {
    Started {
        exe: &'static str,
        app: &'static str,
    },
    Ended {
        exe: &'static str,
        app: &'static str,
    },
}

/// The `detect` state machine (same as the macOS helper): a call starts when a meeting app has
/// an active microphone session and ends after `misses_to_end` polls without one, so a short
/// mute or device switch does not end it.
pub struct Detector {
    misses_to_end: u32,
    active: BTreeMap<&'static str, (&'static str, u32)>,
}

impl Detector {
    pub fn new(misses_to_end: u32) -> Detector {
        Detector {
            misses_to_end: misses_to_end.max(1),
            active: BTreeMap::new(),
        }
    }

    pub fn poll(&mut self, now: &BTreeMap<&'static str, &'static str>) -> Vec<CallEvent> {
        let mut events = Vec::new();
        for (&exe, &app) in now {
            if !self.active.contains_key(exe) {
                events.push(CallEvent::Started { exe, app });
            }
        }
        let misses_to_end = self.misses_to_end;
        self.active.retain(|&exe, (app, misses)| {
            if now.contains_key(exe) {
                *misses = 0;
                return true;
            }
            *misses += 1;
            if *misses >= misses_to_end {
                events.push(CallEvent::Ended { exe, app: *app });
                return false;
            }
            true
        });
        for (&exe, &app) in now {
            self.active.entry(exe).or_insert((app, 0));
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, exe: &str) -> Process {
        Process {
            pid,
            parent,
            exe: exe.into(),
        }
    }

    fn snapshot() -> Vec<Process> {
        vec![
            p(4, 0, "System"),
            p(100, 4, "explorer.exe"),
            // Chrome: a browser process with a renderer and the audio service.
            p(200, 100, "chrome.exe"),
            p(201, 200, "chrome.exe"),
            p(202, 200, "chrome.exe"),
            // New Teams renders in WebView2 children.
            p(300, 100, "ms-teams.exe"),
            p(301, 300, "msedgewebview2.exe"),
            p(302, 301, "msedgewebview2.exe"),
            // Zoom started from a link in Chrome.
            p(400, 200, "Zoom.exe"),
            p(401, 400, "zoom.exe"),
            // A second, separate Chrome profile.
            p(500, 100, "chrome.exe"),
            p(501, 500, "chrome.exe"),
            p(600, 100, "notepad.exe"),
        ]
    }

    #[test]
    fn executables_match_ignoring_case() {
        assert_eq!(app_for_exe("Zoom.exe").map(|m| m.app), Some("zoom"));
        assert_eq!(app_for_exe("MS-TEAMS.EXE").map(|m| m.app), Some("teams"));
        assert_eq!(app_for_exe("msedgewebview2.exe"), None);
        assert!(MEETING_APPS.iter().all(|m| m.exe == m.exe.to_lowercase()));
    }

    #[test]
    fn helper_processes_belong_to_their_meeting_app() {
        let procs = snapshot();
        assert_eq!(meeting_app_of(202, &procs).map(|m| m.exe), Some("chrome.exe"));
        assert_eq!(meeting_app_of(302, &procs).map(|m| m.exe), Some("ms-teams.exe"));
        assert_eq!(meeting_app_of(401, &procs).map(|m| m.exe), Some("zoom.exe"));
        assert_eq!(meeting_app_of(600, &procs), None);
        assert_eq!(meeting_app_of(9999, &procs), None);
    }

    #[test]
    fn parent_cycles_do_not_hang() {
        let procs = vec![p(10, 11, "a.exe"), p(11, 10, "b.exe"), p(12, 12, "c.exe")];
        assert_eq!(meeting_app_of(10, &procs), None);
        assert_eq!(meeting_app_of(12, &procs), None);
        assert!(!in_tree(99, 10, &procs));
    }

    #[test]
    fn calls_are_keyed_by_the_meeting_app() {
        let procs = snapshot();
        let calls = apps_in_call(&[302, 401, 600], &procs);
        assert_eq!(
            calls.into_iter().collect::<Vec<_>>(),
            vec![("ms-teams.exe", "teams"), ("zoom.exe", "zoom")]
        );
        let running: Vec<_> = running_apps(&procs).iter().map(|m| m.exe).collect();
        assert_eq!(running, vec!["zoom.exe", "ms-teams.exe", "chrome.exe"]);
    }

    #[test]
    fn the_capture_root_is_the_top_level_process_of_the_app() {
        let procs = snapshot();
        // Zoom's root is the one started by Chrome, not its child.
        assert_eq!(capture_root("zoom.exe", &procs, &[]), Some(400));
        // Teams' root includes the WebView2 children that render the call.
        assert_eq!(capture_root("ms-teams.exe", &procs, &[]), Some(300));
        assert!(in_tree(300, 302, &procs));
        assert_eq!(capture_root("slack.exe", &procs, &[]), None);
    }

    #[test]
    fn the_instance_using_the_microphone_wins() {
        let procs = snapshot();
        assert_eq!(capture_root("chrome.exe", &procs, &[]), Some(200));
        assert_eq!(capture_root("chrome.exe", &procs, &[501]), Some(500));
        assert_eq!(capture_root("chrome.exe", &procs, &[202]), Some(200));
    }

    #[test]
    fn detection_waits_before_ending_a_call() {
        let mut d = Detector::new(2);
        let zoom: BTreeMap<_, _> = [("zoom.exe", "zoom")].into_iter().collect();
        let none = BTreeMap::new();
        assert_eq!(
            d.poll(&zoom),
            vec![CallEvent::Started {
                exe: "zoom.exe",
                app: "zoom"
            }]
        );
        assert!(d.poll(&zoom).is_empty());
        assert!(d.poll(&none).is_empty());
        assert!(d.poll(&zoom).is_empty());
        assert!(d.poll(&none).is_empty());
        assert_eq!(
            d.poll(&none),
            vec![CallEvent::Ended {
                exe: "zoom.exe",
                app: "zoom"
            }]
        );
        assert!(d.poll(&none).is_empty());
    }
}
