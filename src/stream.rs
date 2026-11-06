//! Reads the PipeWire graph from `pw-dump`: application playback streams, and the default output
//! and input devices.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AudioStream {
    pub node_id: u64,
    pub name: String,
    pub app_name: String,
    pub app_id: String,
    pub binary: String,
    pub description: String,
    pub identity: String,
    pub is_steam_game: bool,
    /// The node is playing right now, rather than idle or suspended.
    pub running: bool,
}

impl AudioStream {
    pub fn detail(&self) -> String {
        [self.app_name.as_str(), self.binary.as_str(), self.description.as_str()]
            .into_iter()
            .filter(|value| !value.is_empty() && *value != self.name)
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// A hardware (or virtual) device PipeWire routes to by default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Device {
    pub description: String,
    pub running: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Graph {
    pub streams: Vec<AudioStream>,
    pub output: Option<Device>,
    pub input: Option<Device>,
}

/// The ALSA plugin names every client "PipeWire ALSA [<program>]"; keep just the program.
pub(crate) fn strip_alsa_wrapper(name: &str) -> &str {
    name.strip_prefix("PipeWire ALSA [")
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(name)
}

fn identity(app_id: &str, app_name: &str, binary: &str, description: &str) -> String {
    if !app_id.is_empty() {
        format!(
            "stream-id:{}:{}:{}",
            app_id.to_lowercase(),
            app_name.to_lowercase(),
            binary.to_lowercase()
        )
    } else {
        format!(
            "stream-app:{}:{}:{}",
            app_name.to_lowercase(),
            binary.to_lowercase(),
            description.to_lowercase()
        )
    }
}

fn is_steam_game(app_name: &str, process_id: Option<u32>) -> bool {
    if app_name.to_ascii_lowercase().starts_with("steam_app_") {
        return true;
    }
    let Some(pid) = process_id else {
        return false;
    };
    fs::read(format!("/proc/{pid}/environ")).ok().is_some_and(|environment| {
        environment.split(|byte| *byte == 0).any(|field| {
            field
                .strip_prefix(b"SteamAppId=")
                .or_else(|| field.strip_prefix(b"SteamGameId="))
                .is_some_and(|value| !value.is_empty())
        })
    })
}

fn of_type<'a>(data: &'a [Value], kind: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
    data.iter().filter(move |item| item["type"].as_str() == Some(kind))
}

fn is_running(item: &Value) -> bool {
    item["info"]["state"].as_str() == Some("running")
}

/// The node name PipeWire's default metadata gives for `key`, such as `default.audio.sink`.
fn default_node_name<'a>(data: &'a [Value], key: &str) -> Option<&'a str> {
    of_type(data, "PipeWire:Interface:Metadata")
        .filter(|item| item["props"]["metadata.name"].as_str() == Some("default"))
        .filter_map(|item| item["metadata"].as_array())
        .flatten()
        .find(|entry| entry["key"].as_str() == Some(key))
        .and_then(|entry| entry["value"]["name"].as_str())
}

fn default_device(data: &[Value], key: &str, class: &str) -> Option<Device> {
    let name = default_node_name(data, key)?;
    let node = of_type(data, "PipeWire:Interface:Node").find(|item| {
        let props = &item["info"]["props"];
        props["node.name"].as_str() == Some(name) && props["media.class"].as_str() == Some(class)
    })?;
    let props = &node["info"]["props"];
    let description = props["node.description"]
        .as_str()
        .or_else(|| props["node.nick"].as_str())
        .unwrap_or(name)
        .trim()
        .to_owned();
    Some(Device { description, running: is_running(node) })
}

/// Parses `pw-dump` output.
pub(crate) fn parse(data: &[Value]) -> Graph {
    let mut seen = HashSet::with_capacity(data.len());
    let mut streams = Vec::new();
    // ALSA-plugin streams only carry the process binary and PID on their owning client.
    let clients: HashMap<u64, &Value> = of_type(data, "PipeWire:Interface:Client")
        .filter_map(|item| Some((item.get("id")?.as_u64()?, &item["info"]["props"])))
        .collect();
    for item in of_type(data, "PipeWire:Interface:Node") {
        let Some(node_id) = item.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let props = &item["info"]["props"];
        if props["media.class"].as_str() != Some("Stream/Output/Audio") || !seen.insert(node_id) {
            continue;
        }
        let client_props = props["client.id"]
            .as_u64()
            .and_then(|id| clients.get(&id).copied())
            .unwrap_or(&Value::Null);
        let prop = |key: &str| {
            props[key]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| client_props[key].as_str())
                .unwrap_or_default()
                .trim()
        };
        let app_name = strip_alsa_wrapper(prop("application.name")).to_owned();
        let app_id = props["application.id"].as_str().unwrap_or_default().trim().to_owned();
        let binary = prop("application.process.binary")
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let description = props["node.description"].as_str().unwrap_or_default().trim().to_owned();
        let name = [&app_name, &binary, &description]
            .into_iter()
            .find(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("Audio stream {node_id}"));
        let process_id = [props, client_props]
            .into_iter()
            .find_map(|props| {
                let id = &props["application.process.id"];
                id.as_u64().or_else(|| id.as_str().and_then(|value| value.parse().ok()))
            })
            .and_then(|value| u32::try_from(value).ok());
        streams.push(AudioStream {
            node_id,
            identity: identity(&app_id, &app_name, &binary, &description),
            is_steam_game: is_steam_game(&app_name, process_id),
            running: is_running(item),
            name,
            app_name,
            app_id,
            binary,
            description,
        });
    }
    streams.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then(left.node_id.cmp(&right.node_id))
    });
    Graph {
        streams,
        output: default_device(data, "default.audio.sink", "Audio/Sink"),
        input: default_device(data, "default.audio.source", "Audio/Source"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: u64, class: &str, state: &str, props: Value) -> Value {
        let mut props = props;
        props["media.class"] = json!(class);
        json!({"id": id, "type": "PipeWire:Interface:Node", "info": {"state": state, "props": props}})
    }

    #[test]
    fn streams_take_names_from_their_alsa_client_and_report_activity() {
        let data = vec![
            json!({"id": 7, "type": "PipeWire:Interface:Client", "info": {"props": {
                "application.process.binary": "/usr/bin/mpv", "application.process.id": "1"}}}),
            node(40, "Stream/Output/Audio", "running", json!({
                "client.id": 7, "application.name": "PipeWire ALSA [mpv]"})),
            node(41, "Stream/Output/Audio", "idle", json!({"node.description": "Firefox"})),
            node(42, "Stream/Input/Audio", "running", json!({"application.name": "OBS"})),
        ];
        let graph = parse(&data);
        let names: Vec<_> = graph.streams.iter().map(|s| (s.name.as_str(), s.running)).collect();
        assert_eq!(names, [("Firefox", false), ("mpv", true)]);
        assert_eq!(graph.streams[1].binary, "mpv");
    }

    #[test]
    fn default_devices_come_from_the_default_metadata() {
        let data = vec![
            json!({"id": 30, "type": "PipeWire:Interface:Metadata", "props": {"metadata.name": "default"},
                "metadata": [
                    {"subject": 0, "key": "default.audio.sink", "value": {"name": "alsa_output.usb"}},
                    {"subject": 0, "key": "default.audio.source", "value": {"name": "alsa_input.usb"}}]}),
            node(50, "Audio/Sink", "running", json!({"node.name": "alsa_output.usb", "node.description": "Scarlett 2i2"})),
            node(51, "Audio/Source", "suspended", json!({"node.name": "alsa_input.usb", "node.nick": "Mic"})),
        ];
        let graph = parse(&data);
        assert_eq!(graph.output, Some(Device { description: "Scarlett 2i2".into(), running: true }));
        assert_eq!(graph.input, Some(Device { description: "Mic".into(), running: false }));
    }

    #[test]
    fn no_default_metadata_means_no_devices() {
        assert_eq!(parse(&[]), Graph::default());
    }
}
