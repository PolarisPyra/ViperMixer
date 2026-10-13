use std::{collections::HashSet, fs, process::Command};
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
}

impl AudioStream {
    pub fn identity(&self) -> String {
        if !self.app_id.is_empty() {
            format!(
                "stream-id:{}:{}:{}",
                self.app_id.to_lowercase(),
                self.app_name.to_lowercase(),
                self.binary.to_lowercase()
            )
        } else {
            format!(
                "stream-app:{}:{}:{}",
                self.app_name.to_lowercase(),
                self.binary.to_lowercase(),
                self.description.to_lowercase()
            )
        }
    }

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

    pub fn is_steam_game(&self) -> bool {
        if self.app_name.to_ascii_lowercase().starts_with("steam_app_") {
            return true;
        }
        let Some(pid) = self.process_id else {
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
        let app_name = props["application.name"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let app_id = props["application.id"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let binary_path = props["application.process.binary"]
            .as_str()
            .unwrap_or_default()
            .trim();
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
        let process_id = props["application.process.id"]
            .as_u64()
            .or_else(|| {
                props["application.process.id"]
                    .as_str()
                    .and_then(|value| value.parse().ok())
            })
            .and_then(|value| u32::try_from(value).ok());
        streams.push(AudioStream {
            node_id,
            name,
            app_name,
            app_id,
            binary,
            description,
            process_id,
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
