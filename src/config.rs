use std::{env, fs, path::PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ui: UiConfig,
    pub behavior: BehaviorConfig,
    pub open: OpenConfig,
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
            Ok(config) => ConfigLoad::Valid { config, path },
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
    }
}
