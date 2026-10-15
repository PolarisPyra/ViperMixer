use serde::Serialize;
use std::{fs, io, path::PathBuf};

pub(crate) const FADER_COUNT: usize = 5;
pub(crate) const STEAM_ALL: &str = "steam-all";
pub(crate) const MIDI_DISCONNECTED: &str = "__disconnected__";

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Config {
    pub applications: Vec<String>,
    pub stream_keys: Vec<String>,
    pub midi_port: String,
    pub mute_notes: Vec<Option<u8>>,
    pub fader_volumes: Vec<f32>,
    pub close_to_tray: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            applications: default_applications(),
            stream_keys: default_stream_keys(),
            midi_port: String::new(),
            mute_notes: default_mute_notes(),
            fader_volumes: default_volumes(),
            close_to_tray: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let value = Self::path()
            .ok()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let Some(object) = value.as_ref().and_then(serde_json::Value::as_object) else {
            return Self::default();
        };

        let applications = fixed_strings(object.get("applications"), FADER_COUNT);
        let stream_keys = fixed_strings(object.get("stream_keys"), FADER_COUNT);
        let midi_port = object
            .get("midi_port")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let notes_value = object
            .get("mute_notes")
            .filter(|value| value.is_array())
            .or_else(|| object.get("mute_ccs"));
        let mute_notes = fixed_notes(notes_value);
        let fader_volumes = fixed_volumes(object.get("fader_volumes"));
        let close_to_tray = object
            .get("close_to_tray")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let mut config = Self {
            applications,
            stream_keys,
            midi_port,
            mute_notes,
            fader_volumes,
            close_to_tray,
        };
        config.normalize();
        config
    }

    pub fn save(&self) -> io::Result<()> {
        let path = Self::path()?;
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "config path has no parent")
        })?;
        fs::create_dir_all(parent)?;
        let temp_path = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::write(&temp_path, bytes)?;
        fs::rename(temp_path, path)
    }

    fn path() -> io::Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        Ok(PathBuf::from(home).join(".config/pipewire-mixer/config.json"))
    }

    fn normalize(&mut self) {
        self.applications.resize(FADER_COUNT, String::new());
        self.applications.truncate(FADER_COUNT);
        self.stream_keys.resize(FADER_COUNT, String::new());
        self.stream_keys.truncate(FADER_COUNT);
        for index in self.mute_notes.len()..FADER_COUNT {
            self.mute_notes.push(u8::try_from(37 + index).ok());
        }
        self.mute_notes.truncate(FADER_COUNT);
        self.fader_volumes.resize(FADER_COUNT, 0.5);
        self.fader_volumes.truncate(FADER_COUNT);
        self.fader_volumes.iter_mut().for_each(|volume| {
            *volume = if volume.is_finite() {
                volume.clamp(0.0, 1.0)
            } else {
                0.5
            };
        });
    }
}

fn default_applications() -> Vec<String> {
    vec![String::new(); FADER_COUNT]
}
fn default_stream_keys() -> Vec<String> {
    vec![String::new(); FADER_COUNT]
}
fn default_mute_notes() -> Vec<Option<u8>> {
    (37..=41).map(Some).collect()
}
fn default_volumes() -> Vec<f32> {
    vec![0.5; FADER_COUNT]
}

fn fixed_strings(value: Option<&serde_json::Value>, length: usize) -> Vec<String> {
    let values = value.and_then(serde_json::Value::as_array);
    (0..length)
        .map(|index| {
            values
                .and_then(|values| values.get(index))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

fn fixed_notes(value: Option<&serde_json::Value>) -> Vec<Option<u8>> {
    let values = value.and_then(serde_json::Value::as_array);
    (0..FADER_COUNT)
        .map(|index| {
            values
                .and_then(|values| values.get(index))
                .and_then(serde_json::Value::as_u64)
                .filter(|note| *note <= 127)
                .map(|note| note as u8)
                .or_else(|| u8::try_from(37 + index).ok())
        })
        .collect()
}

fn fixed_volumes(value: Option<&serde_json::Value>) -> Vec<f32> {
    let values = value.and_then(serde_json::Value::as_array);
    (0..FADER_COUNT)
        .map(|index| {
            values
                .and_then(|values| values.get(index))
                .and_then(serde_json::Value::as_f64)
                .map(|volume| volume as f32)
                .filter(|volume| volume.is_finite())
                .map(|volume| volume.clamp(0.0, 1.0))
                .unwrap_or(0.5)
        })
        .collect()
}
