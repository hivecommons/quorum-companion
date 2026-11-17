//! What the helper reads from the PipeWire graph, and the decisions it takes from it. Plain
//! logic over the JSON of `pw-dump` and `pactl --format=json`, tested on every platform.
//!
//! Every app stream is a node: `Stream/Output/Audio` for what the app plays (the call audio the
//! helper captures), `Stream/Input/Audio` for what it records (a meeting app using the
//! microphone means a call is on). Nodes carry the app's `application.process.binary`, which
//! [`crate::apps`] maps to a meeting app.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::{json, Value};

use crate::apps;

/// Which way an app stream goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// The app plays audio (`Stream/Output/Audio`): the call channel.
    Output,
    /// The app records audio (`Stream/Input/Audio`): it is using a microphone.
    Input,
}

/// One meeting app stream in the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamNode {
    /// `object.serial`, what `pw-record --target` takes (it is never reused, unlike ids).
    pub serial: u64,
    pub direction: Direction,
    pub app_id: &'static str,
    /// The node is processing audio (state `running`), not idle or suspended.
    pub running: bool,
}

/// A number that may be written as a JSON number or a string (pactl writes properties as
/// strings, pw-dump writes `object.serial` as a number).
fn number(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The meeting app a set of stream properties belongs to.
fn app_of(props: &Value) -> Option<&'static apps::MeetingApp> {
    let binary = props
        .get("application.process.binary")
        .and_then(Value::as_str);
    let name = props.get("application.name").and_then(Value::as_str);
    binary
        .and_then(apps::by_binary)
        .or_else(|| name.and_then(apps::by_binary))
}

/// The meeting app streams in a `pw-dump` snapshot. Anything that is not a meeting app's
/// stream node (devices, links, the helper's own recorders) is skipped; bad JSON gives none.
pub fn parse_pw_dump(json: &str) -> Vec<StreamNode> {
    let Ok(Value::Array(objects)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let null = Value::Null;
    let mut nodes = Vec::new();
    for object in &objects {
        if object.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(id) = number(object.get("id")) else {
            continue;
        };
        let Some(info) = object.get("info") else {
            continue;
        };
        let props = info.get("props").unwrap_or(&null);
        let direction = match props.get("media.class").and_then(Value::as_str) {
            Some("Stream/Output/Audio") => Direction::Output,
            Some("Stream/Input/Audio") => Direction::Input,
            _ => continue,
        };
        let Some(app) = app_of(props) else {
            continue;
        };
        nodes.push(StreamNode {
            serial: number(props.get("object.serial")).unwrap_or(id),
            direction,
            app_id: app.id,
            running: info.get("state").and_then(Value::as_str) == Some("running"),
        });
    }
    nodes
}

/// Meeting apps that are recording (using a microphone) right now: id to hive app name.
pub fn apps_in_call(nodes: &[StreamNode]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for node in nodes {
        if node.direction != Direction::Input || !node.running {
            continue;
        }
        if let Some(app) = apps::by_id(node.app_id) {
            out.insert(app.id.to_string(), app.app.to_string());
        }
    }
    out
}

/// Meeting apps with any stream in the graph (they are running even if `/proc` hides them,
/// e.g. in a sandbox).
pub fn apps_with_streams(nodes: &[StreamNode]) -> BTreeSet<&'static str> {
    nodes.iter().map(|n| n.app_id).collect()
}

/// The app's output stream to capture: a running one first, then the newest.
pub fn best_output<'a>(nodes: &'a [StreamNode], app_id: &str) -> Option<&'a StreamNode> {
    nodes
        .iter()
        .filter(|n| is_output_of(n, app_id))
        .max_by_key(|n| (n.running, n.serial))
}

fn is_output_of(node: &StreamNode, app_id: &str) -> bool {
    node.direction == Direction::Output && node.app_id == app_id
}

/// The output stream to capture next, given the one being captured (by serial). The helper
/// stays on its stream while it exists, and moves when it is gone or when it is silent (not
/// running) while another of the app's streams plays: meeting apps open a new stream when the
/// person changes the speaker, browsers open one per tab.
pub fn next_target(current: Option<u64>, nodes: &[StreamNode], app_id: &str) -> Option<u64> {
    let best = best_output(nodes, app_id);
    let is_current = |n: &&StreamNode| Some(n.serial) == current && is_output_of(n, app_id);
    match (nodes.iter().find(is_current), best) {
        (Some(cur), Some(best)) if !cur.running && best.running => Some(best.serial),
        (Some(cur), _) => Some(cur.serial),
        (None, best) => best.map(|n| n.serial),
    }
}

/// How long a PipeWire recorder may stay silent on a stream that is playing before the helper
/// switches to the PulseAudio fallback.
pub const NO_DATA_GRACE: Duration = Duration::from_secs(6);

/// Whether to give up on `pw-record` and record through pipewire-pulse instead: it exited
/// before giving any audio (an old `pw-record` that cannot write raw audio to stdout, or a
/// session manager that refused the link), or it gave nothing for [`NO_DATA_GRACE`] while the
/// stream it targets is playing (the link was never made).
pub fn should_fall_back(bytes: u64, elapsed: Duration, exited: bool, playing: bool) -> bool {
    bytes == 0 && (exited || (playing && elapsed >= NO_DATA_GRACE))
}

/// The pipewire-pulse sink-input index of a stream, from `pactl --format=json list
/// sink-inputs`: the one with the stream's `object.serial`, else the app's newest uncorked
/// sink-input. `None` when the JSON is unusable or nothing matches.
pub fn sink_input_index(json: &str, serial: u64, app_id: &str) -> Option<u64> {
    let Ok(Value::Array(inputs)) = serde_json::from_str::<Value>(json) else {
        return None;
    };
    let props = |input: &Value| input.get("properties").cloned().unwrap_or_default();
    let exact = inputs
        .iter()
        .find(|&input| number(props(input).get("object.serial")) == Some(serial));
    if let Some(input) = exact {
        return number(input.get("index"));
    }
    inputs
        .iter()
        .filter(|&input| app_of(&props(input)).map(|a| a.id) == Some(app_id))
        .filter_map(|input| {
            let corked = input.get("corked") == Some(&Value::Bool(true));
            number(input.get("index")).map(|index| (!corked, index))
        })
        .max()
        .map(|(_, index)| index)
}

/// A change `detect` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Started { id: String, app: String },
    Ended { id: String, app: String },
}

impl Event {
    /// The JSON line the app reads (same keys as the macOS helper).
    pub fn to_json(&self) -> Value {
        let (event, id, app) = match self {
            Event::Started { id, app } => ("call_started", id, app),
            Event::Ended { id, app } => ("call_ended", id, app),
        };
        json!({ "event": event, "bundle_id": id, "app": app })
    }
}

/// Turns polls of the apps in a call into started / ended events. A call ends only after
/// `misses_to_end` polls in a row without the app recording, so a short mute or a device
/// switch does not end it.
pub struct Detector {
    misses_to_end: u32,
    /// App id to (hive app name, polls missed so far).
    active: BTreeMap<String, (String, u32)>,
}

impl Detector {
    pub fn new(misses_to_end: u32) -> Detector {
        Detector {
            misses_to_end: misses_to_end.max(1),
            active: BTreeMap::new(),
        }
    }

    pub fn step(&mut self, now: &BTreeMap<String, String>) -> Vec<Event> {
        let mut events = Vec::new();
        let mut ended = Vec::new();
        for (id, (_, misses)) in self.active.iter_mut() {
            if now.contains_key(id) {
                *misses = 0;
            } else {
                *misses += 1;
                if *misses >= self.misses_to_end {
                    ended.push(id.clone());
                }
            }
        }
        for id in ended {
            if let Some((app, _)) = self.active.remove(&id) {
                events.push(Event::Ended { id, app });
            }
        }
        for (id, app) in now {
            if let Entry::Vacant(slot) = self.active.entry(id.clone()) {
                slot.insert((app.clone(), 0));
                events.push(Event::Started {
                    id: id.clone(),
                    app: app.clone(),
                });
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed `pw-dump` snapshot: a Chrome call (one idle and one playing output stream, a
    /// recording stream), Zoom open but quiet, a sink, a link, the helper's own recorder, and
    /// a node without props.
    const DUMP: &str = r#"[
      {"id": 30, "type": "PipeWire:Interface:Node", "info": {"state": "running",
        "props": {"media.class": "Audio/Sink", "object.serial": 30, "node.name": "alsa_output"}}},
      {"id": 61, "type": "PipeWire:Interface:Node", "info": {"state": "idle",
        "props": {"media.class": "Stream/Output/Audio", "object.serial": 90,
                  "application.process.binary": "chrome", "application.name": "Google Chrome"}}},
      {"id": 62, "type": "PipeWire:Interface:Node", "info": {"state": "running",
        "props": {"media.class": "Stream/Output/Audio", "object.serial": "105",
                  "application.process.binary": "chrome"}}},
      {"id": 63, "type": "PipeWire:Interface:Node", "info": {"state": "running",
        "props": {"media.class": "Stream/Input/Audio", "object.serial": 106,
                  "application.process.binary": "chrome"}}},
      {"id": 70, "type": "PipeWire:Interface:Node", "info": {"state": "suspended",
        "props": {"media.class": "Stream/Output/Audio", "object.serial": 120,
                  "application.name": "zoom"}}},
      {"id": 71, "type": "PipeWire:Interface:Node", "info": {"state": "running",
        "props": {"media.class": "Stream/Input/Audio", "object.serial": 121,
                  "application.process.binary": "pw-record"}}},
      {"id": 72, "type": "PipeWire:Interface:Link", "info": {"state": "active", "props": {}}},
      {"id": 73, "type": "PipeWire:Interface:Node", "info": {"state": "running"}},
      {"id": 74, "type": "PipeWire:Interface:Node"}
    ]"#;

    fn node(serial: u64, direction: Direction, app_id: &'static str, running: bool) -> StreamNode {
        StreamNode {
            serial,
            direction,
            app_id,
            running,
        }
    }

    #[test]
    fn pw_dump_keeps_meeting_app_streams_only() {
        let nodes = parse_pw_dump(DUMP);
        let expected = [
            StreamNode {
                serial: 90,
                direction: Direction::Output,
                app_id: "chrome",
                running: false,
            },
            StreamNode {
                serial: 105,
                direction: Direction::Output,
                app_id: "chrome",
                running: true,
            },
            StreamNode {
                serial: 106,
                direction: Direction::Input,
                app_id: "chrome",
                running: true,
            },
            StreamNode {
                serial: 120,
                direction: Direction::Output,
                app_id: "zoom",
                running: false,
            },
        ];
        assert_eq!(nodes, expected);
    }

    #[test]
    fn bad_dumps_give_no_nodes() {
        assert!(parse_pw_dump("").is_empty());
        assert!(parse_pw_dump("{}").is_empty());
        assert!(parse_pw_dump("[1, null, \"x\"]").is_empty());
    }

    #[test]
    fn a_serial_falls_back_to_the_id() {
        let dump = r#"[{"id": 9, "type": "PipeWire:Interface:Node", "info": {"state": "idle",
          "props": {"media.class": "Stream/Output/Audio",
                    "application.process.binary": "slack"}}}]"#;
        assert_eq!(parse_pw_dump(dump)[0].serial, 9);
    }

    #[test]
    fn calls_are_apps_recording_now() {
        let nodes = parse_pw_dump(DUMP);
        let in_call = apps_in_call(&nodes);
        let expected = BTreeMap::from([("chrome".to_string(), "browser".to_string())]);
        assert_eq!(in_call, expected);
        let with_streams: Vec<&str> = apps_with_streams(&nodes).into_iter().collect();
        assert_eq!(with_streams, ["chrome", "zoom"]);
        let idle_mic = [node(5, Direction::Input, "zoom", false)];
        assert!(apps_in_call(&idle_mic).is_empty());
    }

    #[test]
    fn the_playing_stream_is_captured_first() {
        let nodes = parse_pw_dump(DUMP);
        assert_eq!(best_output(&nodes, "chrome").map(|n| n.serial), Some(105));
        assert_eq!(best_output(&nodes, "zoom").map(|n| n.serial), Some(120));
        assert_eq!(best_output(&nodes, "slack"), None);
        let quiet = [
            node(3, Direction::Output, "zoom", false),
            node(8, Direction::Output, "zoom", false),
            node(9, Direction::Input, "zoom", true),
        ];
        assert_eq!(best_output(&quiet, "zoom").map(|n| n.serial), Some(8));
    }

    #[test]
    fn the_target_stays_until_it_goes_or_another_stream_plays() {
        let both_quiet = [
            node(3, Direction::Output, "zoom", false),
            node(8, Direction::Output, "zoom", false),
        ];
        assert_eq!(next_target(None, &both_quiet, "zoom"), Some(8));
        assert_eq!(next_target(Some(3), &both_quiet, "zoom"), Some(3));
        let other_plays = [
            node(3, Direction::Output, "zoom", false),
            node(8, Direction::Output, "zoom", true),
        ];
        assert_eq!(next_target(Some(3), &other_plays, "zoom"), Some(8));
        let current_plays = [
            node(3, Direction::Output, "zoom", true),
            node(8, Direction::Output, "zoom", true),
        ];
        assert_eq!(next_target(Some(3), &current_plays, "zoom"), Some(3));
        let gone = [node(8, Direction::Output, "zoom", false)];
        assert_eq!(next_target(Some(3), &gone, "zoom"), Some(8));
        assert_eq!(next_target(Some(3), &[], "zoom"), None);
        // A serial of another app or of a recording stream is not a target.
        let mixed = [
            node(3, Direction::Input, "zoom", true),
            node(4, Direction::Output, "slack", true),
        ];
        assert_eq!(next_target(Some(3), &mixed, "zoom"), None);
        assert_eq!(next_target(Some(4), &mixed, "zoom"), None);
    }

    #[test]
    fn fallback_only_when_pw_record_gives_nothing() {
        let early = Duration::from_secs(1);
        assert!(should_fall_back(0, early, true, false));
        assert!(!should_fall_back(0, early, false, true));
        assert!(should_fall_back(0, NO_DATA_GRACE, false, true));
        // A quiet stream gives no audio even when linked: wait for it.
        assert!(!should_fall_back(0, NO_DATA_GRACE * 10, false, false));
        // Audio arrived once: the backend works, later restarts stay on it.
        assert!(!should_fall_back(4, NO_DATA_GRACE, true, true));
    }

    #[test]
    fn sink_inputs_match_by_serial_then_by_app() {
        let pactl = r#"[
          {"index": 41, "corked": false, "properties": {"object.serial": "90",
            "application.process.binary": "chrome"}},
          {"index": 42, "corked": true, "properties": {"object.serial": "95",
            "application.process.binary": "chrome"}},
          {"index": 43, "corked": false, "properties": {"application.process.binary": "chrome"}},
          {"index": 50, "corked": false, "properties": {"application.process.binary": "slack"}}
        ]"#;
        assert_eq!(sink_input_index(pactl, 95, "chrome"), Some(42));
        assert_eq!(sink_input_index(pactl, 1, "chrome"), Some(43));
        assert_eq!(sink_input_index(pactl, 1, "slack"), Some(50));
        assert_eq!(sink_input_index(pactl, 1, "zoom"), None);
        assert_eq!(sink_input_index("Failed to connect", 90, "chrome"), None);
    }

    fn calls(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(id, app)| (id.to_string(), app.to_string()))
            .collect()
    }

    fn started(id: &str, app: &str) -> Event {
        Event::Started {
            id: id.into(),
            app: app.into(),
        }
    }

    fn ended(id: &str, app: &str) -> Event {
        Event::Ended {
            id: id.into(),
            app: app.into(),
        }
    }

    #[test]
    fn detection_has_a_one_poll_grace() {
        let mut d = Detector::new(2);
        let zoom = calls(&[("zoom", "zoom")]);
        let none = calls(&[]);
        assert_eq!(d.step(&zoom), [started("zoom", "zoom")]);
        assert!(d.step(&zoom).is_empty());
        // One poll without the microphone (a mute, a device switch) is not the end.
        assert!(d.step(&none).is_empty());
        assert!(d.step(&zoom).is_empty());
        assert!(d.step(&none).is_empty());
        assert_eq!(d.step(&none), [ended("zoom", "zoom")]);
        assert!(d.step(&none).is_empty());
        // A new call of the same app starts again.
        assert_eq!(d.step(&zoom), [started("zoom", "zoom")]);
    }

    #[test]
    fn detection_tracks_apps_separately() {
        let mut d = Detector::new(1);
        let both = calls(&[("chrome", "browser"), ("slack", "slack")]);
        let events = d.step(&both);
        assert_eq!(
            events,
            [started("chrome", "browser"), started("slack", "slack")]
        );
        let chrome = calls(&[("chrome", "browser")]);
        assert_eq!(d.step(&chrome), [ended("slack", "slack")]);
    }

    #[test]
    fn events_are_the_macos_json_lines() {
        let line = started("zoom", "zoom").to_json().to_string();
        let expected = r#"{"app":"zoom","bundle_id":"zoom","event":"call_started"}"#;
        assert_eq!(line, expected);
        let line = ended("chrome", "browser").to_json();
        assert_eq!(line["event"], "call_ended");
        assert_eq!(line["bundle_id"], "chrome");
    }
}
