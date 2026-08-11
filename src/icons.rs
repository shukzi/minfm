use unicode_width::UnicodeWidthStr;

use crate::{
    config::IconConfig,
    entry::{EntryKind, FileEntry},
};

#[derive(Clone, Copy)]
struct ThemeIcons {
    directory_closed: &'static str,
    directory_open: &'static str,
    file: &'static str,
    text: &'static str,
    code: &'static str,
    image: &'static str,
    audio: &'static str,
    video: &'static str,
    archive: &'static str,
    executable: &'static str,
    symlink: &'static str,
    block_device: &'static str,
    other: &'static str,
}

const PROJECT_ICONS: ThemeIcons = ThemeIcons {
    directory_closed: "󰉋",
    directory_open: "󰉖",
    file: "󰈙",
    text: "󰈙",
    code: "󰅩",
    image: "󰋩",
    audio: "󰎆",
    video: "󰕧",
    archive: "󰏗",
    executable: "󰆍",
    symlink: "󰌷",
    block_device: "󰋊",
    other: "󰋼",
};

pub struct Icons<'a> {
    config: &'a IconConfig,
}

impl<'a> Icons<'a> {
    pub fn new(config: &'a IconConfig) -> Self {
        Self { config }
    }

    pub fn entry(&self, entry: &FileEntry, expanded: bool) -> &str {
        match entry.kind {
            EntryKind::Directory if expanded => self.directory_open(),
            EntryKind::Directory => self.directory_closed(),
            EntryKind::Symlink => {
                self.resolve(&self.config.overrides.symlink, PROJECT_ICONS.symlink)
            }
            EntryKind::BlockDevice => self.resolve(
                &self.config.overrides.block_device,
                PROJECT_ICONS.block_device,
            ),
            EntryKind::Other => self.resolve(&self.config.overrides.other, PROJECT_ICONS.other),
            EntryKind::File if entry.mode & 0o111 != 0 => {
                self.resolve(&self.config.overrides.executable, PROJECT_ICONS.executable)
            }
            EntryKind::File => self.file_icon(entry),
        }
    }

    pub fn header_trash(&self) -> &str {
        self.resolve(&self.config.overrides.trash, "󰩹")
    }

    pub fn header_info(&self) -> &str {
        self.resolve(&self.config.overrides.info, "󰋼")
    }

    pub fn header_devices(&self) -> &str {
        self.resolve(&self.config.overrides.devices, "󰍹")
    }

    pub fn header_partitions(&self) -> &str {
        self.resolve(&self.config.overrides.partitions, "󰋊")
    }

    pub fn header_sort(&self) -> &str {
        self.resolve(&self.config.overrides.sort, "󰒺")
    }

    pub fn slot(icon: &str) -> String {
        let padding = 3usize.saturating_sub(UnicodeWidthStr::width(icon));
        format!("{icon}{}", " ".repeat(padding))
    }

    fn directory_closed(&self) -> &str {
        self.resolve(
            &self.config.overrides.directory_closed,
            PROJECT_ICONS.directory_closed,
        )
    }

    fn directory_open(&self) -> &str {
        self.resolve(
            &self.config.overrides.directory_open,
            PROJECT_ICONS.directory_open,
        )
    }

    fn file_icon(&self, entry: &FileEntry) -> &str {
        let extension = entry
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("txt" | "md" | "markdown" | "rst" | "pdf" | "doc" | "docx" | "odt") => {
                self.resolve(&self.config.overrides.text, PROJECT_ICONS.text)
            }
            Some(
                "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "go" | "java" | "kt" | "kts" | "py"
                | "rb" | "php" | "lua" | "sh" | "bash" | "zsh" | "fish" | "js" | "jsx" | "ts"
                | "tsx" | "html" | "css" | "sql" | "toml" | "yaml" | "yml" | "json" | "xml",
            ) => self.resolve(&self.config.overrides.code, PROJECT_ICONS.code),
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff") => {
                self.resolve(&self.config.overrides.image, PROJECT_ICONS.image)
            }
            Some("mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" | "opus") => {
                self.resolve(&self.config.overrides.audio, PROJECT_ICONS.audio)
            }
            Some("mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v") => {
                self.resolve(&self.config.overrides.video, PROJECT_ICONS.video)
            }
            Some("zip" | "tar" | "gz" | "bz2" | "xz" | "zst" | "7z" | "rar") => {
                self.resolve(&self.config.overrides.archive, PROJECT_ICONS.archive)
            }
            _ => self.resolve(&self.config.overrides.file, PROJECT_ICONS.file),
        }
    }

    fn resolve<'b>(&self, override_value: &'b Option<String>, fallback: &'static str) -> &'b str {
        override_value.as_deref().unwrap_or(fallback)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::IconOverrides;

    fn entry(name: &str, kind: EntryKind, mode: u32) -> FileEntry {
        FileEntry {
            path: PathBuf::from(name),
            name: name.into(),
            kind,
            size: 0,
            mode,
            modified: None,
            selected: false,
        }
    }

    #[test]
    fn project_icons_cover_only_file_manager_categories() {
        let config = IconConfig::default();
        let icons = Icons::new(&config);
        assert_eq!(
            icons.entry(&entry("folder", EntryKind::Directory, 0), false),
            "󰉋"
        );
        assert_eq!(
            icons.entry(&entry("folder", EntryKind::Directory, 0), true),
            "󰉖"
        );
        assert_eq!(
            icons.entry(&entry("notes.md", EntryKind::File, 0), false),
            "󰈙"
        );
        assert_eq!(
            icons.entry(&entry("main.rs", EntryKind::File, 0), false),
            "󰅩"
        );
        assert_eq!(
            icons.entry(&entry("photo.png", EntryKind::File, 0), false),
            "󰋩"
        );
        assert_eq!(
            icons.entry(&entry("song.flac", EntryKind::File, 0), false),
            "󰎆"
        );
        assert_eq!(
            icons.entry(&entry("movie.mkv", EntryKind::File, 0), false),
            "󰕧"
        );
        assert_eq!(
            icons.entry(&entry("backup.tar", EntryKind::File, 0), false),
            "󰏗"
        );
        assert_eq!(
            icons.entry(&entry("run", EntryKind::File, 0o755), false),
            "󰆍"
        );
        assert_eq!(
            icons.entry(&entry("link", EntryKind::Symlink, 0), false),
            "󰌷"
        );
    }

    #[test]
    fn overrides_are_resolved_centrally() {
        let config = IconConfig {
            overrides: IconOverrides {
                directory_closed: Some("D".into()),
                trash: Some("X".into()),
                ..IconOverrides::default()
            },
        };
        let icons = Icons::new(&config);
        assert_eq!(
            icons.entry(&entry("folder", EntryKind::Directory, 0), false),
            "D"
        );
        assert_eq!(
            icons.entry(&entry("main.rs", EntryKind::File, 0), false),
            "󰅩"
        );
        assert_eq!(icons.header_trash(), "X");
    }

    #[test]
    fn icon_slots_have_a_stable_three_cell_width() {
        assert_eq!(UnicodeWidthStr::width(Icons::slot("T").as_str()), 3);
        assert_eq!(UnicodeWidthStr::width(Icons::slot("{}").as_str()), 3);
    }
}
