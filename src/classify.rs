//! Application classification: window class -> display name + category.
//!
//! Only the window class is ever stored; window titles are used (later,
//! optionally) but never persisted by the tracker. Classification is fully
//! configurable: built-in tables cover common apps, and `display_names` in
//! the config overrides or extends them.

use std::collections::{HashMap, HashSet};

use crate::config::Tracking;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Editor,
    Browser,
    Terminal,
    Media,
    Other,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Editor => "editor",
            Category::Browser => "browser",
            Category::Terminal => "terminal",
            Category::Media => "media",
            Category::Other => "other",
        }
    }
}

/// Common window classes and their friendly names.
pub const BUILTIN_DISPLAY_NAMES: &[(&str, &str)] = &[
    ("code", "Visual Studio Code"),
    ("code-oss", "VS Code OSS"),
    ("codium", "VSCodium"),
    ("jetbrains-idea", "IntelliJ IDEA"),
    ("firefox", "Firefox"),
    ("chromium", "Chromium"),
    ("brave-browser", "Brave"),
    ("google-chrome", "Chrome"),
    ("microsoft-edge", "Microsoft Edge"),
    ("librewolf", "LibreWolf"),
    ("zen", "Zen Browser"),
    ("vivaldi-stable", "Vivaldi"),
    ("kitty", "Kitty"),
    ("foot", "Foot"),
    ("footclient", "Foot"),
    ("alacritty", "Alacritty"),
    ("ghostty", "Ghostty"),
    ("wezterm", "WezTerm"),
    ("konsole", "Konsole"),
    ("spotify", "Spotify"),
    ("mpv", "mpv"),
    ("vlc", "VLC"),
];

const EDITOR_CLASSES: &[&str] = &["code", "code-oss", "codium", "vscodium", "jetbrains-idea", "idea"];
const BROWSER_CLASSES: &[&str] = &[
    "firefox",
    "chromium",
    "brave-browser",
    "google-chrome",
    "microsoft-edge",
    "librewolf",
    "zen",
    "vivaldi-stable",
    "opera",
];
const TERMINAL_CLASSES: &[&str] = &[
    "kitty",
    "foot",
    "footclient",
    "alacritty",
    "ghostty",
    "wezterm",
    "konsole",
    "gnome-terminal-server",
    "xterm",
    "st-256color",
];
const MEDIA_CLASSES: &[&str] = &["spotify", "mpv", "vlc", "rhythmbox"];

#[derive(Debug, Clone, PartialEq)]
pub struct AppInfo {
    pub class: String,
    pub display_name: String,
    pub category: Category,
    pub excluded: bool,
}

#[derive(Debug, Clone)]
pub struct Classifier {
    overrides: HashMap<String, String>,
    excluded: HashSet<String>,
}

impl Classifier {
    pub fn from_config(tracking: &Tracking) -> Self {
        Classifier {
            overrides: tracking
                .display_names
                .iter()
                .map(|(k, v)| (normalize(k), v.clone()))
                .collect(),
            excluded: tracking.excluded_apps.iter().map(|s| normalize(s)).collect(),
        }
    }

    pub fn classify(&self, class: &str) -> AppInfo {
        let norm = normalize(class);
        let excluded = self.excluded.contains(&norm);
        let display_name = self
            .overrides
            .get(&norm)
            .cloned()
            .or_else(|| {
                BUILTIN_DISPLAY_NAMES
                    .iter()
                    .find(|(c, _)| *c == norm)
                    .map(|(_, n)| n.to_string())
            })
            .unwrap_or_else(|| class.trim().to_string());
        let category = if EDITOR_CLASSES.contains(&norm.as_str()) {
            Category::Editor
        } else if BROWSER_CLASSES.contains(&norm.as_str()) {
            Category::Browser
        } else if TERMINAL_CLASSES.contains(&norm.as_str()) {
            Category::Terminal
        } else if MEDIA_CLASSES.contains(&norm.as_str()) {
            Category::Media
        } else {
            Category::Other
        };
        AppInfo {
            class: norm,
            display_name,
            category,
            excluded,
        }
    }
}

/// Window classes are matched case-insensitively.
pub fn normalize(class: &str) -> String {
    class.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier_with(display: &[(&str, &str)], excluded: &[&str]) -> Classifier {
        Classifier::from_config(&Tracking {
            display_names: display
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            excluded_apps: excluded.iter().map(|s| s.to_string()).collect(),
            ..Tracking::default()
        })
    }

    #[test]
    fn builtin_mapping() {
        let c = classifier_with(&[], &[]);
        let info = c.classify("code");
        assert_eq!(info.display_name, "Visual Studio Code");
        assert_eq!(info.category, Category::Editor);
        assert!(!info.excluded);
    }

    #[test]
    fn builtin_is_case_insensitive() {
        let c = classifier_with(&[], &[]);
        assert_eq!(c.classify("Code").display_name, "Visual Studio Code");
        assert_eq!(c.classify("FIREFOX").display_name, "Firefox");
    }

    #[test]
    fn config_override_wins() {
        let c = classifier_with(&[("firefox", "My Browser")], &[]);
        let info = c.classify("firefox");
        assert_eq!(info.display_name, "My Browser");
        assert_eq!(info.category, Category::Browser);
    }

    #[test]
    fn unknown_class_keeps_raw_name() {
        let c = classifier_with(&[], &[]);
        let info = c.classify("SomeWeirdApp");
        assert_eq!(info.display_name, "SomeWeirdApp");
        assert_eq!(info.category, Category::Other);
    }

    #[test]
    fn exclusion_respects_case() {
        let c = classifier_with(&[], &["Spotify"]);
        assert!(c.classify("spotify").excluded);
        assert!(!c.classify("kitty").excluded);
    }
}
