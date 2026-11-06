//! Settings in `~/.config/viper-mixer/config.json`. The file keeps the keys the egui version of
//! Viper Mixer wrote, so its assignments, levels and mute notes carry over, and adds the learned
//! fader and mute controls.

use crate::midi::{Control, Kind};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::{fs, io};

pub(crate) const FADER_COUNT: usize = 5;
pub(crate) const STEAM_ALL: &str = "steam-all";
pub(crate) const OUTPUT: &str = "default-output";
pub(crate) const INPUT: &str = "default-input";
pub(crate) const MIDI_DISCONNECTED: &str = "__disconnected__";
/// Where a strip starts before it has a saved level.
const DEFAULT_VOLUME: f32 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Config {
    pub applications: Vec<String>,
    pub stream_keys: Vec<String>,
    /// The MIDI input without its ALSA numbers (see `midi::stable_port_name`), "" for Automatic,
    /// or `MIDI_DISCONNECTED`.
    pub midi_port: String,
    pub fader_controls: Vec<Option<Control>>,
    pub mute_controls: Vec<Option<Control>>,
    pub fader_volumes: Vec<f32>,
    pub close_to_tray: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self::from_value(&Value::Null)
    }
}

impl Config {
    pub fn load() -> Self {
        path()
            .ok()
            .and_then(read)
            .or_else(|| legacy_path().ok().and_then(read))
            .map_or_else(Self::default, |value| Self::from_value(&value))
    }

    pub fn save(&self) -> io::Result<()> {
        let path = path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Written beside and renamed over, so a crash never leaves half a file.
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.to_value()).map_err(io::Error::other)?;
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)
    }

    pub fn from_value(value: &Value) -> Self {
        let empty = Map::new();
        let object = value.as_object().unwrap_or(&empty);
        let strings = |key| fixed(object.get(key), |value| value.as_str().map(str::to_owned), |_| String::new());
        // The egui version kept only mute notes (and before that `mute_ccs`, also notes).
        let legacy_notes = object.get("mute_notes").filter(|value| value.is_array()).or_else(|| object.get("mute_ccs"));
        let mute_controls = match object.get("mute_controls") {
            Some(value) => fixed(Some(value), |value| Some(control_from(value)), |_| None),
            None => fixed(
                legacy_notes,
                // As the egui version read them: a missing or unreadable note is the default one.
                |value| value.as_u64().filter(|note| *note <= 127).map(|note| Some(Control::note(note as u8))),
                |index| Some(Control::note(37 + index as u8)),
            ),
        };
        Self {
            applications: strings("applications"),
            stream_keys: strings("stream_keys"),
            midi_port: crate::midi::stable_port_name(object.get("midi_port").and_then(Value::as_str).unwrap_or_default())
                .to_owned(),
            fader_controls: fixed(
                object.get("fader_controls"),
                |value| Some(control_from(value)),
                |index| Some(Control::cc(index as u8 + 1)),
            ),
            mute_controls,
            fader_volumes: fixed(
                object.get("fader_volumes"),
                |value| value.as_f64().map(|volume| volume as f32).filter(|volume| volume.is_finite()).map(|volume| volume.clamp(0.0, 1.0)),
                |_| DEFAULT_VOLUME,
            ),
            close_to_tray: object.get("close_to_tray").and_then(Value::as_bool).unwrap_or(true),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "applications": self.applications,
            "stream_keys": self.stream_keys,
            "midi_port": self.midi_port,
            // For the egui version, which reads only notes.
            "mute_notes": self.mute_controls.iter()
                .map(|control| control.filter(|control| control.kind == Kind::Note).map(|control| control.number))
                .collect::<Vec<_>>(),
            "fader_controls": self.fader_controls.iter().map(control_to).collect::<Vec<_>>(),
            "mute_controls": self.mute_controls.iter().map(control_to).collect::<Vec<_>>(),
            "fader_volumes": self.fader_volumes,
            "close_to_tray": self.close_to_tray,
        })
    }

    /// Binds `control` to a strip's fader or mute, taking it off any other strip first, so one
    /// hardware control never drives two things.
    pub fn bind(&mut self, index: usize, mute: bool, control: Control) {
        for slot in self.fader_controls.iter_mut().chain(self.mute_controls.iter_mut()) {
            if slot.is_some_and(|bound| bound == control) {
                *slot = None;
            }
        }
        let list = if mute { &mut self.mute_controls } else { &mut self.fader_controls };
        list[index] = Some(control);
    }
}

/// `FADER_COUNT` entries: each read with `item`, or `default` where the list is short or the
/// entry unreadable.
fn fixed<T>(value: Option<&Value>, item: impl Fn(&Value) -> Option<T>, default: impl Fn(usize) -> T) -> Vec<T> {
    let values = value.and_then(Value::as_array);
    (0..FADER_COUNT)
        .map(|index| values.and_then(|values| values.get(index)).and_then(&item).unwrap_or_else(|| default(index)))
        .collect()
}

fn control_from(value: &Value) -> Option<Control> {
    let kind = match value["kind"].as_str()? {
        "cc" => Kind::Cc,
        "note" => Kind::Note,
        _ => return None,
    };
    let number = u8::try_from(value["number"].as_u64()?).ok().filter(|number| *number <= 127)?;
    let channel = value["channel"].as_u64().and_then(|channel| u8::try_from(channel).ok()).filter(|channel| *channel < 16);
    Some(Control { kind, channel, number })
}

fn control_to(control: &Option<Control>) -> Value {
    control.map_or(Value::Null, |control| {
        json!({
            "kind": match control.kind { Kind::Cc => "cc", Kind::Note => "note" },
            "channel": control.channel,
            "number": control.number,
        })
    })
}

fn read(path: PathBuf) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn config_home() -> io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config"))
}

fn path() -> io::Result<PathBuf> {
    Ok(config_home()?.join("viper-mixer/config.json"))
}

fn legacy_path() -> io::Result<PathBuf> {
    Ok(config_home()?.join("pipewire-mixer/config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_egui_versions_file_carries_over() {
        let old = json!({
            "applications": ["Firefox", "steam-all"],
            "stream_keys": ["stream-id:firefox:firefox:firefox"],
            "midi_port": "nanoKONTROL2:nanoKONTROL2 _ CTRL 24:0",
            "mute_notes": [40, null, 200],
            "fader_volumes": [0.25, 7.0, "loud"],
            "close_to_tray": false,
        });
        let config = Config::from_value(&old);
        assert_eq!(config.applications, ["Firefox", "steam-all", "", "", ""]);
        assert_eq!(config.midi_port, "nanoKONTROL2:nanoKONTROL2 _ CTRL");
        assert_eq!(config.mute_controls[..4], [Some(Control::note(40)), Some(Control::note(38)), Some(Control::note(39)), Some(Control::note(40))]);
        assert_eq!(config.fader_controls[4], Some(Control::cc(5)));
        assert_eq!(config.fader_volumes, [0.25, 1.0, 0.5, 0.5, 0.5]);
        assert!(!config.close_to_tray);
    }

    #[test]
    fn saved_settings_read_back_the_same() {
        let mut config = Config::default();
        config.bind(2, false, Control { channel: Some(9), ..Control::cc(70) });
        config.fader_controls[0] = None;
        config.midi_port = MIDI_DISCONNECTED.into();
        assert_eq!(Config::from_value(&config.to_value()), config);
    }

    #[test]
    fn binding_a_control_takes_it_off_other_strips() {
        let mut config = Config::default();
        config.bind(0, true, Control::cc(2));
        assert_eq!(config.fader_controls[1], None, "CC 2 was fader 2's");
        assert_eq!(config.mute_controls[0], Some(Control::cc(2)));
        config.bind(3, true, Control::cc(2));
        assert_eq!(config.mute_controls[0], None);
    }
}
