use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub verbose: bool,
    pub quiet: bool,
    pub sanitize_only: bool,
    pub unique_only: bool,
    pub sort_only: bool,
    pub combine_commit: bool,
    pub output: Option<PathBuf>,
    pub check: bool,
    pub backup: bool,
    pub backup_path: Option<PathBuf>,
    pub sort_main_group: bool,
    pub watch: bool,
}

impl Config {
    pub fn run_unique(&self) -> bool {
        !self.sort_only
    }

    pub fn run_sort(&self) -> bool {
        !self.unique_only
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct TomlConfig {
    #[serde(default)]
    pub verbose: Option<bool>,
    #[serde(default)]
    pub quiet: Option<bool>,
    #[serde(default, rename = "sanitize-only")]
    pub sanitize_only: Option<bool>,
    #[serde(default, rename = "unique-only")]
    pub unique_only: Option<bool>,
    #[serde(default, rename = "sort-only")]
    pub sort_only: Option<bool>,
    #[serde(default, rename = "combine-commit")]
    pub combine_commit: Option<bool>,
    #[serde(default, rename = "sort-main-group")]
    pub sort_main_group: Option<bool>,
    #[serde(default)]
    pub backup: Option<bool>,
    #[serde(default, rename = "backup-path")]
    pub backup_path: Option<String>,
    #[serde(default)]
    pub watch: Option<bool>,
}

impl TomlConfig {
    pub fn into_config(self) -> Config {
        let mut config = Config::default();
        if let Some(v) = self.verbose { config.verbose = v; }
        if let Some(v) = self.quiet { config.quiet = v; }
        if let Some(v) = self.sanitize_only { config.sanitize_only = v; }
        if let Some(v) = self.unique_only { config.unique_only = v; }
        if let Some(v) = self.sort_only { config.sort_only = v; }
        if let Some(v) = self.combine_commit { config.combine_commit = v; }
        if let Some(v) = self.sort_main_group { config.sort_main_group = v; }
        if let Some(v) = self.backup { config.backup = v; }
        if let Some(v) = self.backup_path { config.backup_path = Some(v.into()); }
        if let Some(v) = self.watch { config.watch = v; }
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_config_parses_correctly() {
        let toml_str = r#"
verbose = true
sanitize-only = false
sort-main-group = true
backup = true
backup-path = "custom.bak"
"#;
        let cfg: TomlConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.verbose, Some(true));
        assert_eq!(cfg.sanitize_only, Some(false));
        assert_eq!(cfg.sort_main_group, Some(true));
        assert_eq!(cfg.backup, Some(true));
        assert_eq!(cfg.backup_path, Some("custom.bak".into()));
    }

    #[test]
    fn toml_config_into_config_works() {
        let toml = TomlConfig {
            verbose: Some(true),
            backup: Some(true),
            ..TomlConfig::default()
        };
        let config = toml.into_config();
        assert!(config.verbose);
        assert!(config.backup);
        assert!(!config.check);
    }
}
