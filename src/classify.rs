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
    /// AI coding agents and assistant desktop apps (Cursor, Windsurf, Claude…).
    Agent,
    Media,
    Other,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Editor => "editor",
            Category::Browser => "browser",
            Category::Terminal => "terminal",
            Category::Agent => "agent",
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
    ("zed", "Zed"),
    ("sublime_text", "Sublime Text"),
    ("cursor", "Cursor"),
    ("cursor-nightly", "Cursor Nightly"),
    ("windsurf", "Windsurf"),
    ("windsurf-next", "Windsurf Next"),
    ("trae", "Trae"),
    ("claude", "Claude"),
    ("chatgpt", "ChatGPT"),
    ("gemini", "Gemini"),
    ("org.omarchy.agent", "Omarchy Agent"),
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

const EDITOR_CLASSES: &[&str] = &[
    "code",
    "code-oss",
    "codium",
    "vscodium",
    "jetbrains-idea",
    "idea",
    "zed",
    "sublime_text",
    "android-studio",
];
// AI coding agents and assistant apps. Everything JetBrains ships is an
// editor too, so classes like "jetbrains-pycharm" are caught by the
// prefix rule in `classify_base` rather than listed one by one.
const AGENT_CLASSES: &[&str] = &[
    "cursor",
    "cursor-nightly",
    "windsurf",
    "windsurf-next",
    "trae",
    "trae-cn",
    "claude",
    "chatgpt",
    "gemini",
    // Omarchy's built-in AI agent app.
    "org.omarchy.agent",
];
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
    /// Whether this app's usage is recorded at all: on the developer-app
    /// allowlist (via `tracked_categories` / `tracked_apps`) and not in
    /// `excluded_apps`. `excluded` wins over the allowlist.
    pub tracked: bool,
}

#[derive(Debug, Clone)]
pub struct Classifier {
    overrides: HashMap<String, String>,
    excluded: HashSet<String>,
    tracked_categories: HashSet<String>,
    tracked_apps: HashSet<String>,
    editor_projects: bool,
}

impl Classifier {
    pub fn from_config(tracking: &Tracking) -> Self {
        Classifier {
            overrides: tracking
                .display_names
                .iter()
                .map(|(k, v)| (normalize(k), v.clone()))
                .collect(),
            excluded: tracking
                .excluded_apps
                .iter()
                .map(|s| normalize(s))
                .collect(),
            tracked_categories: tracking
                .tracked_categories
                .iter()
                .map(|s| normalize(s))
                .collect(),
            tracked_apps: tracking.tracked_apps.iter().map(|s| normalize(s)).collect(),
            editor_projects: tracking.editor_project_names,
        }
    }

    /// Classify by window class only.
    pub fn classify(&self, class: &str) -> AppInfo {
        self.classify_window(class, "")
    }

    /// Classify by window class and title. When `editor_project_names` is
    /// enabled, editor windows get their project basename appended to the
    /// display name (e.g. "Visual Studio Code · atw") — file names and
    /// paths are never stored.
    pub fn classify_window(&self, class: &str, title: &str) -> AppInfo {
        let mut info = self.classify_base(class);
        if self.editor_projects && info.category == Category::Editor {
            if let Some(project) = editor_project_from_title(title) {
                info.display_name = format!("{} · {}", info.display_name, project);
            }
        }
        info
    }

    fn classify_base(&self, class: &str) -> AppInfo {
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
            .or_else(|| brave_pwa_display_name(&norm))
            .unwrap_or_else(|| class.trim().to_string());
        let category = if norm.starts_with("jetbrains-") || EDITOR_CLASSES.contains(&norm.as_str())
        {
            Category::Editor
        // Brave PWA windows ("brave-<origin>__-Default", e.g. a ChatGPT web
        // app installed as a browser app) are browser windows too.
        } else if norm.starts_with("brave-") || BROWSER_CLASSES.contains(&norm.as_str()) {
            Category::Browser
        } else if TERMINAL_CLASSES.contains(&norm.as_str()) {
            Category::Terminal
        } else if AGENT_CLASSES.contains(&norm.as_str()) {
            Category::Agent
        } else if MEDIA_CLASSES.contains(&norm.as_str()) {
            Category::Media
        } else {
            Category::Other
        };
        // The allowlist decides what gets recorded: only developer-relevant
        // categories (plus explicitly listed classes), never excluded apps.
        let tracked = !excluded
            && (self.tracked_categories.contains(category.as_str())
                || self.tracked_apps.contains(&norm));
        AppInfo {
            class: norm,
            display_name,
            category,
            excluded,
            tracked,
        }
    }
}

/// Extract the project basename from a VS Code-style window title:
/// "file.rs - myproject - Visual Studio Code" → "myproject".
/// Returns None when the title does not have the expected shape. Only the
/// folder basename is ever extracted — never file names or paths.
pub fn editor_project_from_title(title: &str) -> Option<String> {
    let parts: Vec<&str> = title.split(" - ").map(str::trim).collect();
    if parts.len() < 2 {
        return None;
    }
    let candidate = parts[parts.len() - 2];
    if candidate.is_empty() || candidate.len() > 64 {
        return None;
    }
    Some(candidate.to_string())
}

/// Brave PWA windows use classes like "brave-chatgpt.com__-Default".
/// Show the site ("chatgpt.com") rather than the raw class. Takes the
/// normalized (lowercased) class. Config `display_names` entries still win.
fn brave_pwa_display_name(norm: &str) -> Option<String> {
    let rest = norm.strip_prefix("brave-")?;
    let name = rest.strip_suffix("__-default").unwrap_or(rest);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
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

    #[test]
    fn dev_apps_are_tracked_by_default() {
        let c = classifier_with(&[], &[]);
        assert!(c.classify("code").tracked);
        assert!(c.classify("Firefox").tracked);
        assert!(c.classify("kitty").tracked);
        // AI agents are their own category.
        let cursor = c.classify("cursor");
        assert_eq!(cursor.category, Category::Agent);
        assert_eq!(cursor.display_name, "Cursor");
        assert!(cursor.tracked);
    }

    #[test]
    fn non_dev_apps_are_not_tracked_by_default() {
        let c = classifier_with(&[], &[]);
        assert!(!c.classify("spotify").tracked); // media
        assert!(!c.classify("SomeWeirdApp").tracked); // unclassified
        assert!(!c.classify("org.gnome.Nautilus").tracked);
    }

    #[test]
    fn tracked_apps_extends_the_allowlist() {
        let tracking = Tracking {
            tracked_apps: vec!["spotify".to_string()],
            ..Tracking::default()
        };
        let c = Classifier::from_config(&tracking);
        assert!(c.classify("Spotify").tracked);
        assert!(!c.classify("vlc").tracked);
    }

    #[test]
    fn tracked_categories_can_be_narrowed() {
        let tracking = Tracking {
            tracked_categories: vec!["editor".to_string()],
            ..Tracking::default()
        };
        let c = Classifier::from_config(&tracking);
        assert!(c.classify("code").tracked);
        assert!(!c.classify("firefox").tracked);
        assert!(!c.classify("kitty").tracked);
    }

    #[test]
    fn exclusion_wins_over_the_allowlist() {
        let tracking = Tracking {
            excluded_apps: vec!["code".to_string()],
            ..Tracking::default()
        };
        let c = Classifier::from_config(&tracking);
        let info = c.classify("code");
        assert!(info.excluded);
        assert!(!info.tracked);
    }

    #[test]
    fn jetbrains_prefix_is_editor() {
        let c = classifier_with(&[], &[]);
        let info = c.classify("jetbrains-pycharm");
        assert_eq!(info.category, Category::Editor);
        assert!(info.tracked);
    }

    #[test]
    fn omarchy_agent_is_an_agent() {
        let c = classifier_with(&[], &[]);
        let info = c.classify("org.omarchy.agent");
        assert_eq!(info.category, Category::Agent);
        assert_eq!(info.display_name, "Omarchy Agent");
        assert!(info.tracked);
    }

    #[test]
    fn brave_pwa_windows_are_browsers() {
        let c = classifier_with(&[], &[]);
        let info = c.classify("brave-chatgpt.com__-Default");
        assert_eq!(info.category, Category::Browser);
        assert_eq!(info.display_name, "chatgpt.com");
        assert!(info.tracked);
    }

    #[test]
    fn editor_project_from_title_parses_vscode_shape() {
        assert_eq!(
            editor_project_from_title("main.rs - atw - Visual Studio Code"),
            Some("atw".to_string())
        );
        assert_eq!(
            editor_project_from_title("● notes.md - diary - Visual Studio Code"),
            Some("diary".to_string())
        );
        assert_eq!(editor_project_from_title("Visual Studio Code"), None);
        assert_eq!(editor_project_from_title(""), None);
    }

    #[test]
    fn editor_project_appended_only_when_enabled() {
        let c = classifier_with(&[], &[]);
        assert_eq!(
            c.classify_window("code", "main.rs - atw - Visual Studio Code")
                .display_name,
            "Visual Studio Code"
        );
        let tracking = Tracking {
            editor_project_names: true,
            ..Tracking::default()
        };
        let c = Classifier::from_config(&tracking);
        assert_eq!(
            c.classify_window("code", "main.rs - atw - Visual Studio Code")
                .display_name,
            "Visual Studio Code · atw"
        );
        // Non-editor windows are never suffixed.
        assert_eq!(
            c.classify_window("firefox", "Example page - Mozilla Firefox")
                .display_name,
            "Firefox"
        );
    }
}
