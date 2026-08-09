use std::{collections::HashMap, env, fs, path::PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{de, Deserialize, Deserializer};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ui: UiConfig,
    pub behavior: BehaviorConfig,
    pub open: OpenConfig,
    pub hotkeys: Box<HotkeyConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub show_hidden: bool,
    pub directories_first: bool,
    pub sort: SortSetting,
    pub reverse_sort: bool,
    pub show_size: bool,
    pub show_permissions: bool,
    pub show_modified: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortSetting {
    Name,
    Extension,
    Size,
    Modified,
    Type,
    Permissions,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BehaviorConfig {
    pub verify_copies: bool,
    pub read_only: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenConfig {
    pub editor: String,
    pub opener: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct KeyChord {
    code: KeyCode,
    control: bool,
    alt: bool,
}

#[derive(Debug, Clone)]
pub struct KeyBinding {
    chord: KeyChord,
    display: String,
}

impl KeyBinding {
    fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.is_empty() {
            return Err("hotkeys cannot be empty".into());
        }
        let parts = value.split('+').map(str::trim).collect::<Vec<_>>();
        let (modifiers, key) = parts.split_at(parts.len().saturating_sub(1));
        let mut control = false;
        let mut alt = false;
        for modifier in modifiers {
            if modifier.eq_ignore_ascii_case("ctrl") || modifier.eq_ignore_ascii_case("control") {
                if control {
                    return Err(format!("duplicate Ctrl modifier in hotkey {value:?}"));
                }
                control = true;
            } else if modifier.eq_ignore_ascii_case("alt") {
                if alt {
                    return Err(format!("duplicate Alt modifier in hotkey {value:?}"));
                }
                alt = true;
            } else {
                return Err(format!(
                    "unsupported modifier {modifier:?} in hotkey {value:?}"
                ));
            }
        }
        let key = key
            .first()
            .copied()
            .ok_or_else(|| format!("hotkey {value:?} has no key"))?;
        let code = match key.to_ascii_lowercase().as_str() {
            "space" => KeyCode::Char(' '),
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "enter" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "backspace" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            lower if lower.starts_with('f') && lower[1..].parse::<u8>().is_ok() => {
                let number = lower[1..].parse::<u8>().unwrap_or_default();
                if !(1..=12).contains(&number) {
                    return Err(format!("function key in {value:?} must be F1 through F12"));
                }
                KeyCode::F(number)
            }
            _ => {
                let mut characters = key.chars();
                let character = characters
                    .next()
                    .ok_or_else(|| format!("hotkey {value:?} has no key"))?;
                if characters.next().is_some() {
                    return Err(format!(
                        "hotkey {value:?} must use one character or a named key"
                    ));
                }
                KeyCode::Char(character)
            }
        };
        Ok(Self {
            chord: KeyChord { code, control, alt },
            display: value.to_owned(),
        })
    }

    pub fn matches(&self, event: KeyEvent) -> bool {
        self.chord.code == event.code
            && self.chord.control == event.modifiers.contains(KeyModifiers::CONTROL)
            && self.chord.alt == event.modifiers.contains(KeyModifiers::ALT)
    }

    pub fn display(&self) -> &str {
        &self.display
    }
}

impl<'de> Deserialize<'de> for KeyBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

fn key(value: &str) -> KeyBinding {
    KeyBinding::parse(value).expect("built-in hotkey defaults must be valid")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeyConfig {
    pub force_quit: KeyBinding,
    pub quit: KeyBinding,
    pub down: KeyBinding,
    pub up: KeyBinding,
    pub expand: KeyBinding,
    pub collapse: KeyBinding,
    pub toggle_view: KeyBinding,
    pub select: KeyBinding,
    pub hidden: KeyBinding,
    pub sort: KeyBinding,
    pub reverse_sort: KeyBinding,
    pub go_to: KeyBinding,
    pub search: KeyBinding,
    pub search_filesystem: KeyBinding,
    pub rename: KeyBinding,
    pub refresh: KeyBinding,
    pub create_directory: KeyBinding,
    pub create_file: KeyBinding,
    pub copy: KeyBinding,
    pub cut: KeyBinding,
    pub paste: KeyBinding,
    pub trash: KeyBinding,
    pub quick_trash: KeyBinding,
    pub trash_bin: KeyBinding,
    pub info: KeyBinding,
    pub help: KeyBinding,
    pub open: KeyBinding,
    pub edit: KeyBinding,
    pub apps: KeyBinding,
    pub devices: KeyBinding,
    pub network_shares: KeyBinding,
    pub device_eject: KeyBinding,
    pub device_action: KeyBinding,
    pub device_unmount: KeyBinding,
    pub network_add: KeyBinding,
    pub network_disconnect: KeyBinding,
    pub network_forget: KeyBinding,
    pub partition_actions: KeyBinding,
    pub restore: KeyBinding,
    pub permanent_delete: KeyBinding,
    pub quick_permanent_delete: KeyBinding,
    pub clear_trash: KeyBinding,
    pub confirm_yes: KeyBinding,
    pub confirm_no: KeyBinding,
    pub overwrite: KeyBinding,
    pub skip: KeyBinding,
    pub abort: KeyBinding,
    pub config_reload: KeyBinding,
    pub config_edit: KeyBinding,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            force_quit: key("Ctrl+c"),
            quit: key("q"),
            down: key("j"),
            up: key("k"),
            expand: key("l"),
            collapse: key("h"),
            toggle_view: key("v"),
            select: key("Space"),
            hidden: key("."),
            sort: key("s"),
            reverse_sort: key("S"),
            go_to: key("g"),
            search: key("/"),
            search_filesystem: key("F"),
            rename: key("r"),
            refresh: key("r"),
            create_directory: key("a"),
            create_file: key("n"),
            copy: key("c"),
            cut: key("x"),
            paste: key("p"),
            trash: key("d"),
            quick_trash: key("D"),
            trash_bin: key("T"),
            info: key("I"),
            help: key("?"),
            open: key("o"),
            edit: key("e"),
            apps: key("M"),
            devices: key("m"),
            network_shares: key("N"),
            device_eject: key("e"),
            device_action: key("m"),
            device_unmount: key("u"),
            network_add: key("a"),
            network_disconnect: key("u"),
            network_forget: key("d"),
            partition_actions: key("a"),
            restore: key("r"),
            permanent_delete: key("d"),
            quick_permanent_delete: key("D"),
            clear_trash: key("C"),
            confirm_yes: key("y"),
            confirm_no: key("n"),
            overwrite: key("o"),
            skip: key("s"),
            abort: key("a"),
            config_reload: key("r"),
            config_edit: key("e"),
        }
    }
}

impl HotkeyConfig {
    fn validate(&self) -> Result<(), String> {
        self.validate_context(
            "browser",
            &[
                ("force_quit", &self.force_quit),
                ("quit", &self.quit),
                ("down", &self.down),
                ("up", &self.up),
                ("expand", &self.expand),
                ("collapse", &self.collapse),
                ("toggle_view", &self.toggle_view),
                ("select", &self.select),
                ("hidden", &self.hidden),
                ("sort", &self.sort),
                ("reverse_sort", &self.reverse_sort),
                ("go_to", &self.go_to),
                ("search", &self.search),
                ("search_filesystem", &self.search_filesystem),
                ("rename", &self.rename),
                ("create_directory", &self.create_directory),
                ("create_file", &self.create_file),
                ("copy", &self.copy),
                ("cut", &self.cut),
                ("paste", &self.paste),
                ("trash", &self.trash),
                ("quick_trash", &self.quick_trash),
                ("trash_bin", &self.trash_bin),
                ("info", &self.info),
                ("help", &self.help),
                ("open", &self.open),
                ("edit", &self.edit),
                ("apps", &self.apps),
                ("devices", &self.devices),
                ("network_shares", &self.network_shares),
            ],
        )?;
        self.validate_context(
            "trash",
            &[
                ("quit", &self.quit),
                ("down", &self.down),
                ("up", &self.up),
                ("select", &self.select),
                ("restore", &self.restore),
                ("permanent_delete", &self.permanent_delete),
                ("quick_permanent_delete", &self.quick_permanent_delete),
                ("clear_trash", &self.clear_trash),
            ],
        )?;
        self.validate_context(
            "devices",
            &[
                ("quit", &self.quit),
                ("down", &self.down),
                ("up", &self.up),
                ("refresh", &self.refresh),
                ("device_eject", &self.device_eject),
                ("device_action", &self.device_action),
                ("device_unmount", &self.device_unmount),
            ],
        )?;
        self.validate_context(
            "network shares",
            &[
                ("quit", &self.quit),
                ("down", &self.down),
                ("up", &self.up),
                ("expand", &self.expand),
                ("refresh", &self.refresh),
                ("network_add", &self.network_add),
                ("network_disconnect", &self.network_disconnect),
                ("network_forget", &self.network_forget),
            ],
        )?;
        self.validate_context(
            "partition manager",
            &[
                ("quit", &self.quit),
                ("down", &self.down),
                ("up", &self.up),
                ("expand", &self.expand),
                ("collapse", &self.collapse),
                ("refresh", &self.refresh),
                ("partition_actions", &self.partition_actions),
                ("apps", &self.apps),
            ],
        )?;
        self.validate_context(
            "apps launcher",
            &[
                ("quit", &self.quit),
                ("down", &self.down),
                ("up", &self.up),
                ("expand", &self.expand),
                ("apps", &self.apps),
            ],
        )?;
        self.validate_context(
            "search results",
            &[
                ("down", &self.down),
                ("up", &self.up),
                ("expand", &self.expand),
                ("search", &self.search),
                ("search_filesystem", &self.search_filesystem),
            ],
        )?;
        self.validate_context(
            "configuration error",
            &[
                ("quit", &self.quit),
                ("config_reload", &self.config_reload),
                ("config_edit", &self.config_edit),
            ],
        )?;
        self.validate_context(
            "yes/no confirmation",
            &[
                ("confirm_yes", &self.confirm_yes),
                ("confirm_no", &self.confirm_no),
            ],
        )?;
        self.validate_context(
            "overwrite confirmation",
            &[
                ("overwrite", &self.overwrite),
                ("skip", &self.skip),
                ("abort", &self.abort),
            ],
        )?;
        Ok(())
    }

    fn validate_context(
        &self,
        context: &str,
        bindings: &[(&str, &KeyBinding)],
    ) -> Result<(), String> {
        let mut assigned = HashMap::<KeyChord, &str>::new();
        for (name, binding) in bindings {
            if matches!(
                binding.chord.code,
                KeyCode::Enter
                    | KeyCode::Esc
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
            ) {
                return Err(format!(
                    "hotkey {name:?} cannot use reserved universal key {:?}",
                    binding.display()
                ));
            }
            if let Some(existing) = assigned.insert(binding.chord.clone(), name) {
                return Err(format!(
                    "hotkeys {existing:?} and {name:?} both use {:?} in the {context} context",
                    binding.display()
                ));
            }
        }
        Ok(())
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            show_hidden: false,
            directories_first: true,
            sort: SortSetting::Name,
            reverse_sort: false,
            show_size: true,
            show_permissions: true,
            show_modified: true,
        }
    }
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            verify_copies: true,
            read_only: false,
        }
    }
}

impl Default for OpenConfig {
    fn default() -> Self {
        Self {
            editor: "xdg-open".into(),
            opener: "xdg-open".into(),
        }
    }
}

#[derive(Debug)]
pub enum ConfigLoad {
    Valid { config: Config, path: PathBuf },
    Invalid { path: PathBuf, error: String },
}

pub fn config_path() -> PathBuf {
    if let Some(base) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(base).join("minfm/config.toml");
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/minfm/config.toml")
}

pub fn load() -> ConfigLoad {
    let path = config_path();
    load_from(path)
}

pub fn load_from(path: PathBuf) -> ConfigLoad {
    match fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(config) => match config.hotkeys.validate() {
                Ok(()) => ConfigLoad::Valid { config, path },
                Err(error) => ConfigLoad::Invalid { path, error },
            },
            Err(error) => ConfigLoad::Invalid {
                path,
                error: error.to_string(),
            },
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConfigLoad::Valid {
            config: Config::default(),
            path,
        },
        Err(error) => ConfigLoad::Invalid {
            path,
            error: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_uses_safe_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let result = load_from(temp.path().join("missing.toml"));
        let ConfigLoad::Valid { config, .. } = result else {
            panic!("missing configuration must use defaults");
        };
        assert_eq!(config.open.opener, "xdg-open");
        assert_eq!(config.open.editor, "xdg-open");
    }

    #[test]
    fn invalid_config_blocks_startup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "[ui]\nsort = 'surprise'\n").unwrap();
        assert!(matches!(load_from(path), ConfigLoad::Invalid { .. }));
    }

    #[test]
    fn distributed_example_config_is_valid() {
        let parsed = toml::from_str::<Config>(include_str!("../config.example.toml"));
        assert!(
            parsed.is_ok(),
            "example config must remain loadable: {parsed:?}"
        );
    }

    #[test]
    fn partial_config_preserves_values_and_fills_missing_defaults() {
        let config = toml::from_str::<Config>("[open]\neditor = 'nano'\n").unwrap();
        assert_eq!(config.open.editor, "nano");
        assert_eq!(config.open.opener, "xdg-open");
        assert!(config.behavior.verify_copies);
        assert_eq!(config.hotkeys.apps.display(), "M");
    }

    #[test]
    fn custom_hotkeys_parse_named_keys_and_modifiers() {
        let config = toml::from_str::<Config>(
            "[hotkeys]\napps = 'F2'\nforce_quit = 'Alt+x'\nselect = 'Tab'\n",
        )
        .unwrap();
        config.hotkeys.validate().unwrap();
        assert!(config
            .hotkeys
            .apps
            .matches(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)));
        assert!(config
            .hotkeys
            .force_quit
            .matches(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)));
        assert!(config
            .hotkeys
            .select
            .matches(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
    }

    #[test]
    fn duplicate_hotkeys_are_rejected_within_the_same_context() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "[hotkeys]\nquit = 'j'\n").unwrap();
        let ConfigLoad::Invalid { error, .. } = load_from(path) else {
            panic!("duplicate browser hotkeys must be rejected");
        };
        assert!(error.contains("both use"));
        assert!(error.contains("browser"));
    }

    #[test]
    fn universal_control_keys_cannot_be_shadowed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "[hotkeys]\napps = 'Enter'\n").unwrap();
        let ConfigLoad::Invalid { error, .. } = load_from(path) else {
            panic!("reserved universal controls must be rejected");
        };
        assert!(error.contains("reserved universal key"));
    }
}
