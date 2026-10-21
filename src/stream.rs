use std::{
    collections::{HashMap, HashSet},
    fs,
    process::Command,
};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AudioStream {
    pub node_id: u64,
    pub name: String,
    pub app_name: String,
    pub app_id: String,
    pub binary: String,
    pub description: String,
    pub process_id: Option<u32>,
    pub identity: String,
    pub is_steam_game: bool,
}

impl AudioStream {
    pub fn detail(&self) -> String {
        [
            self.app_name.as_str(),
            self.binary.as_str(),
            self.description.as_str(),
        ]
        .into_iter()
        .filter(|value| !value.is_empty() && *value != self.name)
        .collect::<Vec<_>>()
        .join(" • ")
    }
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
    fs::read(format!("/proc/{pid}/environ"))
        .ok()
        .is_some_and(|environment| {
            environment.split(|byte| *byte == 0).any(|field| {
                field
                    .strip_prefix(b"SteamAppId=")
                    .or_else(|| field.strip_prefix(b"SteamGameId="))
                    .is_some_and(|value| !value.is_empty())
            })
        })
}

#[derive(Debug, Error)]
pub(crate) enum StreamError {
    #[error("could not run pw-dump: {0}")]
    Command(#[from] std::io::Error),
    #[error("pw-dump returned an error: {0}")]
    PipeWire(String),
    #[error("could not parse PipeWire data: {0}")]
    Json(#[from] serde_json::Error),
}

pub(crate) fn discover() -> Result<Vec<AudioStream>, StreamError> {
    let output = Command::new("pw-dump").output()?;
    if !output.status.success() {
        return Err(StreamError::PipeWire(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let data: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
    parse(&data)
}

pub(crate) fn parse(data: &[serde_json::Value]) -> Result<Vec<AudioStream>, StreamError> {
    let mut seen = HashSet::with_capacity(data.len());
    let mut streams = Vec::new();
    // ALSA-plugin streams only carry the process binary and PID on their owning client.
    let clients: HashMap<u64, &serde_json::Value> = data
        .iter()
        .filter(|item| {
            item.get("type").and_then(serde_json::Value::as_str)
                == Some("PipeWire:Interface:Client")
        })
        .filter_map(|item| Some((item.get("id")?.as_u64()?, &item["info"]["props"])))
        .collect();
    for item in data {
        if item.get("type").and_then(serde_json::Value::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(node_id) = item.get("id").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        let props = &item["info"]["props"];
        if props["media.class"].as_str() != Some("Stream/Output/Audio") || !seen.insert(node_id) {
            continue;
        }
        let client_props = props["client.id"]
            .as_u64()
            .and_then(|id| clients.get(&id).copied())
            .unwrap_or(&serde_json::Value::Null);
        let prop = |key: &str| {
            props[key]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| client_props[key].as_str())
                .unwrap_or_default()
                .trim()
        };
        let app_name = strip_alsa_wrapper(prop("application.name")).to_owned();
        let app_id = props["application.id"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let binary_path = prop("application.process.binary");
        let binary = binary_path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let description = props["node.description"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let name = if !app_name.is_empty() {
            app_name.clone()
        } else if !binary.is_empty() {
            binary.clone()
        } else if !description.is_empty() {
            description.clone()
        } else {
            format!("Audio stream {node_id}")
        };
        let process_id = [props, client_props]
            .into_iter()
            .find_map(|props| {
                props["application.process.id"].as_u64().or_else(|| {
                    props["application.process.id"]
                        .as_str()
                        .and_then(|value| value.parse().ok())
                })
            })
            .and_then(|value| u32::try_from(value).ok());
        let identity = identity(&app_id, &app_name, &binary, &description);
        let is_steam_game = is_steam_game(&app_name, process_id);
        streams.push(AudioStream {
            node_id,
            name,
            app_name,
            app_id,
            binary,
            description,
            process_id,
            identity,
            is_steam_game,
        });
    }
    streams.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then(left.node_id.cmp(&right.node_id))
    });
    Ok(streams)
}
