use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct RawConfig {
    handler: String,
    #[serde(default = "default_gesture_timeout_ms")]
    gesture_timeout_ms: u64,
    #[serde(default = "default_log_level")]
    log_level: String,
    #[serde(default)]
    profiles: HashMap<String, String>,
}

fn default_gesture_timeout_ms() -> u64 {
    400
}

fn default_log_level() -> String {
    "info".into()
}

#[derive(Debug)]
pub struct Config {
    pub handler: String,
    pub gesture_timeout_ms: u64,
    pub log_level: String,
    pub profiles: HashMap<u32, String>,
}

impl Config {
    pub fn gesture_timeout(&self) -> Duration {
        Duration::from_millis(self.gesture_timeout_ms)
    }
}

fn parse_config(text: &str) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| format!("invalid config: {e}"))?;
    let mut profiles = HashMap::new();
    for (k, v) in raw.profiles {
        let n: u32 = k
            .parse()
            .map_err(|_| format!("profile key {k:?} is not a valid press count"))?;
        profiles.insert(n, v);
    }
    Ok(Config {
        handler: raw.handler,
        gesture_timeout_ms: raw.gesture_timeout_ms,
        log_level: raw.log_level,
        profiles,
    })
}

pub fn load_config(path: &str) -> Config {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("s1500d: cannot read config {path}: {e}");
        std::process::exit(1);
    });
    parse_config(&text).unwrap_or_else(|e| {
        eprintln!("s1500d: {e}");
        std::process::exit(1);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let toml = r#"
            handler = "/usr/bin/scan.sh"
            gesture_timeout_ms = 500
            log_level = "debug"

            [profiles]
            1 = "standard"
            2 = "legal"
            3 = "photo"
        "#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.handler, "/usr/bin/scan.sh");
        assert_eq!(config.gesture_timeout_ms, 500);
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.profiles.len(), 3);
        assert_eq!(config.profiles[&1], "standard");
        assert_eq!(config.profiles[&2], "legal");
        assert_eq!(config.profiles[&3], "photo");
    }

    #[test]
    fn parse_minimal_config_uses_defaults() {
        let toml = r#"handler = "/bin/handler.sh""#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.gesture_timeout_ms, 400);
        assert_eq!(config.log_level, "info");
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn parse_invalid_profile_key() {
        let toml = r#"
            handler = "/bin/h.sh"
            [profiles]
            abc = "bad"
        "#;
        assert!(parse_config(toml).is_err());
    }

    #[test]
    fn parse_invalid_toml() {
        assert!(parse_config("not valid toml {{{{").is_err());
    }

    #[test]
    fn parse_missing_handler() {
        let toml = r#"
            gesture_timeout_ms = 400
            [profiles]
            1 = "standard"
        "#;
        assert!(parse_config(toml).is_err());
    }

    #[test]
    fn gesture_timeout_conversion() {
        let config = parse_config(r#"handler = "/bin/h.sh""#).unwrap();
        assert_eq!(config.gesture_timeout(), Duration::from_millis(400));
    }
}
