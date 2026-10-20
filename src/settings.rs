//! Viewer settings remembered between runs, stored as `key = value` lines in
//! `$XDG_CONFIG_HOME/gbtui/settings.conf` (usually
//! `~/.config/gbtui/settings.conf`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Show the GC skew track in the interactive viewer.
    pub gc_skew: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { gc_skew: true }
    }
}

impl Settings {
    /// Where settings live, or `None` if neither `XDG_CONFIG_HOME` nor
    /// `HOME` is set.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("gbtui").join("settings.conf"))
    }

    /// Read settings, falling back to defaults if the file is missing or
    /// unreadable. Unknown keys and malformed lines are ignored.
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, self.to_text())
    }

    fn parse(text: &str) -> Self {
        let mut settings = Settings::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() == "gc_skew" {
                if let Ok(on) = value.trim().parse::<bool>() {
                    settings.gc_skew = on;
                }
            }
        }
        settings
    }

    fn to_text(&self) -> String {
        format!(
            "# gbtui settings (written by the viewer; safe to edit or delete)\n\
             gc_skew = {}\n",
            self.gc_skew
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing_or_malformed() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(
            Settings::parse("gc_skew = maybe\nnonsense"),
            Settings::default()
        );
        assert_eq!(
            Settings::load(Path::new("/nonexistent/gbtui/settings.conf")),
            Settings::default()
        );
    }

    #[test]
    fn parses_and_ignores_comments_and_unknown_keys() {
        let text = "# comment\ncolour = blue\n  gc_skew=false  \n";
        assert_eq!(Settings::parse(text), Settings { gc_skew: false });
    }

    #[test]
    fn round_trips_through_a_file() {
        let dir =
            std::env::temp_dir().join(format!("gbtui-settings-{}", std::process::id()));
        let path = dir.join("nested").join("settings.conf");

        let off = Settings { gc_skew: false };
        off.save(&path).unwrap();
        assert_eq!(Settings::load(&path), off);

        Settings::default().save(&path).unwrap();
        assert_eq!(Settings::load(&path), Settings::default());

        fs::remove_dir_all(&dir).unwrap();
    }
}
