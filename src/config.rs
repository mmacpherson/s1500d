use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
    600
}

fn default_log_level() -> String {
    "info".into()
}

/// Recommended gesture window. Shorter windows make multi-press gestures hard
/// to perform; longer ones delay every scan. Values outside it only warn.
pub const GESTURE_TIMEOUT_MS: std::ops::RangeInclusive<u64> = 100..=5000;
const LOG_LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

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

    /// Valid but probably unintended settings.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if !GESTURE_TIMEOUT_MS.contains(&self.gesture_timeout_ms) {
            warnings.push(format!(
                "gesture_timeout_ms = {} is outside the recommended {}..={} ms",
                self.gesture_timeout_ms,
                GESTURE_TIMEOUT_MS.start(),
                GESTURE_TIMEOUT_MS.end()
            ));
        }
        warnings
    }
}

fn parse_config(text: &str) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| format!("invalid config: {e}"))?;
    if raw.handler.trim().is_empty() {
        return Err("handler is empty; set it to the path of an executable script".into());
    }
    if raw.gesture_timeout_ms == 0 {
        return Err("gesture_timeout_ms must be greater than 0".into());
    }
    if !LOG_LEVELS.contains(&raw.log_level.to_ascii_lowercase().as_str()) {
        return Err(format!(
            "log_level = {:?} is not one of {}",
            raw.log_level,
            LOG_LEVELS.join(", ")
        ));
    }
    let mut profiles = HashMap::new();
    for (k, v) in raw.profiles {
        let n: u32 = k
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| format!("profile key {k:?} is not a press count (1, 2, 3, ...)"))?;
        if v.trim().is_empty() {
            return Err(format!("profile {n} has an empty name"));
        }
        profiles.insert(n, v);
    }
    Ok(Config {
        handler: raw.handler,
        gesture_timeout_ms: raw.gesture_timeout_ms,
        log_level: raw.log_level,
        profiles,
    })
}

/// Whether this process's effective user may execute `path`.
fn can_execute(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: c_path is a valid NUL-terminated string for the duration of the call.
    unsafe {
        libc::faccessat(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            libc::X_OK,
            libc::AT_EACCESS,
        ) == 0
    }
}

/// The search path exec uses when PATH is unset (glibc: /bin:/usr/bin).
fn default_search_path() -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    // SAFETY: a null buffer with length 0 asks only for the required size.
    let len = unsafe { libc::confstr(libc::_CS_PATH, std::ptr::null_mut(), 0) };
    if len == 0 {
        return "/bin:/usr/bin".into();
    }
    let mut buf = vec![0u8; len];
    // SAFETY: buf has exactly the length confstr reported, including the NUL.
    unsafe { libc::confstr(libc::_CS_PATH, buf.as_mut_ptr().cast(), len) };
    buf.truncate(len - 1);
    std::ffi::OsString::from_vec(buf)
}

/// Resolve the handler as the daemon will run it: a name without a slash is
/// looked up on PATH (or exec's default search path when PATH is unset),
/// anything else is a path. It must be a regular file this account may
/// execute. The handler itself is never run.
fn check_handler(handler: &str) -> Result<(), String> {
    check_handler_on(handler, std::env::var_os("PATH"))
}

fn check_handler_on(handler: &str, path: Option<std::ffi::OsString>) -> Result<(), String> {
    use std::path::PathBuf;
    let candidates: Vec<PathBuf> = if handler.contains('/') {
        vec![PathBuf::from(handler)]
    } else {
        let path = path.unwrap_or_else(default_search_path);
        std::env::split_paths(&path)
            .map(|dir| dir.join(handler))
            .collect()
    };
    let mut last_err = format!("handler {handler}: not found on PATH");
    for candidate in candidates {
        let shown = candidate.display();
        match std::fs::metadata(&candidate) {
            Err(e) => last_err = format!("handler {shown}: {e}"),
            Ok(meta) if !meta.is_file() => {
                last_err = format!("handler {shown} is not a regular file");
            }
            Ok(_) if can_execute(&candidate) => return Ok(()),
            Ok(_) => {
                last_err = format!(
                    "handler {shown} is not executable by this account (check its mode and owner)"
                );
            }
        }
    }
    Err(last_err)
}

/// Read and validate a config file, including the handler on disk.
pub fn check_config(path: &str) -> Result<Config, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read config {path}: {e}"))?;
    let config = parse_config(&text).map_err(|e| format!("{path}: {e}"))?;
    check_handler(&config.handler)?;
    Ok(config)
}

pub fn load_config(path: &str) -> Config {
    let config = check_config(path).unwrap_or_else(|e| {
        eprintln!("s1500d: {e}");
        std::process::exit(1);
    });
    for warning in config.warnings() {
        eprintln!("s1500d: warning: {warning}");
    }
    config
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
        assert_eq!(config.gesture_timeout_ms, 600);
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
        assert_eq!(config.gesture_timeout(), Duration::from_millis(600));
    }

    fn rejects(toml: &str, needle: &str) {
        let err = parse_config(toml).unwrap_err();
        assert!(err.contains(needle), "{err:?} lacks {needle:?}");
    }

    #[test]
    fn example_config_is_valid() {
        let config = parse_config(include_str!("../contrib/config.toml")).unwrap();
        assert_eq!(config.gesture_timeout_ms, 600);
        assert_eq!(config.profiles.len(), 2);
    }

    #[test]
    fn rejects_unknown_key() {
        rejects(
            "handler = \"/bin/h.sh\"\ngesture_timout_ms = 400",
            "gesture_timout_ms",
        );
    }

    #[test]
    fn rejects_empty_handler() {
        rejects("handler = \" \"", "handler is empty");
    }

    #[test]
    fn gesture_timeout_zero_rejected_others_warn() {
        rejects("handler = \"/h\"\ngesture_timeout_ms = 0", "greater than 0");
        let warned = |ms: u64| {
            !parse_config(&format!("handler = \"/h\"\ngesture_timeout_ms = {ms}"))
                .unwrap()
                .warnings()
                .is_empty()
        };
        assert!(!warned(100) && !warned(600) && !warned(5000));
        assert!(warned(50) && warned(20000));
    }

    #[test]
    fn log_level_must_be_a_level() {
        for level in ["off", "error", "warn", "INFO", "debug", "trace"] {
            assert!(parse_config(&format!("handler = \"/h\"\nlog_level = \"{level}\"")).is_ok());
        }
        rejects("handler = \"/h\"\nlog_level = \"verbose\"", "log_level");
    }

    #[test]
    fn rejects_zero_press_count_and_empty_profile() {
        rejects("handler = \"/h\"\n[profiles]\n0 = \"x\"", "press count");
        rejects("handler = \"/h\"\n[profiles]\n1 = \"\"", "empty name");
    }

    #[test]
    fn handler_must_be_executable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("s1500d-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("handler.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        let path = script.to_str().unwrap();

        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(check_handler(path).unwrap_err().contains("not executable"));
        // Others may execute but the owner may not, so exec fails for the
        // owner. (Root may exec with any x bit set, and so may faccessat.)
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o641)).unwrap();
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            assert!(check_handler(path).unwrap_err().contains("not executable"));
        }
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(check_handler(path).is_ok());
        assert!(check_handler(dir.to_str().unwrap())
            .unwrap_err()
            .contains("not a regular file"));
        assert!(check_handler(dir.join("missing").to_str().unwrap()).is_err());
        // Bare names resolve on PATH, as Command::new does: an unset PATH
        // falls back to exec's default search path; an empty one searches
        // only the current directory.
        assert!(check_handler("sh").is_ok());
        assert!(check_handler_on("sh", None).is_ok());
        assert!(check_handler_on("sh", Some("".into())).is_err());
        assert!(check_handler_on("sh", Some(dir.clone().into_os_string())).is_err());
        assert!(check_handler("s1500d-no-such-handler")
            .unwrap_err()
            .contains("s1500d-no-such-handler"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
