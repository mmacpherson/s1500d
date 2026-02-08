//! s1500d — Bespoke event daemon for the Fujitsu ScanSnap S1500.
//!
//! Monitors hardware status (button presses, paper in feeder) via direct
//! USB communication and fires a handler script on state transitions.
//! Door open/close is detected via USB device presence.
//!
//! # Protocol
//!
//! The S1500 uses vendor-specific USB (class FF:FF:FF) with two bulk endpoints.
//! SCSI commands are wrapped in a 31-byte envelope:
//!
//! ```text
//! byte 0:     0x43  (Fujitsu USB_COMMAND_CODE)
//! bytes 1-18: 0x00  (padding)
//! bytes 19+:  SCSI CDB (up to 12 bytes)
//! ```
//!
//! The protocol is 3-phase: command → data → status (0x53 envelope).
//!
//! GET_HW_STATUS (SCSI 0xC2) returns 12 bytes:
//! - byte\[3\] bit 7: hopper empty (inverted — 1 = empty, 0 = paper present)
//! - byte\[4\] bit 5: scan button physically held
//!
//! Door state is not reported in GET_HW_STATUS because opening/closing the
//! ADF lid powers the scanner on/off, which is a USB connect/disconnect event.
//!
//! # Usage
//!
//! ```sh
//! # Monitor only (log events to stderr/journal):
//! s1500d
//!
//! # Legacy mode — run handler on each raw event:
//! s1500d handler.sh
//!
//! # Config mode — gesture detection + profile dispatch:
//! s1500d -c /etc/s1500d/config.toml
//!
//! # Interactive hardware verification:
//! s1500d --doctor
//! ```

mod config;
mod doctor;

use std::process::Command as ShellCommand;
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use rusb::UsbContext;

use config::{load_config, Config};
use doctor::doctor;

// ── Device constants ──────────────────────────────────────────────────

const VID: u16 = 0x04C5;
const PID: u16 = 0x11A2;
const EP_OUT: u8 = 0x02;
const EP_IN: u8 = 0x81;
const IFACE: u8 = 0;

pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(100);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
const USB_TIMEOUT: Duration = Duration::from_millis(1000);
const STATUS_TIMEOUT: Duration = Duration::from_millis(200);

// ── Fujitsu USB protocol ─────────────────────────────────────────────

/// Wrap a SCSI CDB in the 31-byte Fujitsu USB command envelope.
fn envelope(cdb: &[u8]) -> [u8; 31] {
    let mut buf = [0u8; 31];
    buf[0] = 0x43;
    buf[19..19 + cdb.len()].copy_from_slice(cdb);
    buf
}

/// GET_HW_STATUS CDB: opcode 0xC2, allocation length 12 (at CDB bytes 7-8).
const GHS_CDB: [u8; 10] = [0xC2, 0, 0, 0, 0, 0, 0, 0, 0x0C, 0];

// ── State types ──────────────────────────────────────────────────────

/// Snapshot of scanner hardware state, decoded from GET_HW_STATUS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct State {
    pub(crate) paper: bool,  // paper present in hopper
    pub(crate) button: bool, // scan button physically held down
}

impl State {
    fn from_response(buf: &[u8]) -> Self {
        Self {
            paper: buf.get(3).is_some_and(|&b| b & 0x80 == 0),
            // bit 5 (0x20) = button held; bit 0 (0x01) = button momentary/tap
            button: buf.get(4).is_some_and(|&b| b & 0x21 != 0),
        }
    }
}

/// Events that the daemon can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    DeviceArrived,
    DeviceLeft,
    PaperIn,
    PaperOut,
    ButtonDown,
    ButtonUp,
}

impl Event {
    const fn tag(self) -> &'static str {
        match self {
            Self::DeviceArrived => "device-arrived",
            Self::DeviceLeft => "device-left",
            Self::PaperIn => "paper-in",
            Self::PaperOut => "paper-out",
            Self::ButtonDown => "button-down",
            Self::ButtonUp => "button-up",
        }
    }
}

/// Compare two states and yield the transition events between them.
fn transitions(prev: State, curr: State) -> impl Iterator<Item = Event> {
    [
        (!prev.paper && curr.paper).then_some(Event::PaperIn),
        (prev.paper && !curr.paper).then_some(Event::PaperOut),
        (!prev.button && curr.button).then_some(Event::ButtonDown),
        (prev.button && !curr.button).then_some(Event::ButtonUp),
    ]
    .into_iter()
    .flatten()
}

// ── Gesture state machine ────────────────────────────────────────────

/// Tracks multi-press gestures on the scan button.
///
/// ```text
/// Idle
///   └─ button-down ──→ Pressed(count=1)
///
/// Pressed(n)
///   └─ button-up ────→ Released(n, timestamp)
///
/// Released(n, t)
///   ├─ button-down ──→ Pressed(n+1)       # another press within window
///   └─ timeout ──────→ emit scan(n) → Idle # window expired, fire gesture
/// ```
#[derive(Debug)]
enum GestureState {
    Idle,
    Pressed(u32),
    Released(u32, Instant),
}

// ── USB communication ────────────────────────────────────────────────

/// Open the scanner, returning a claimed device handle.
pub(crate) fn try_open(ctx: &rusb::Context) -> Option<rusb::DeviceHandle<rusb::Context>> {
    let handle = ctx.open_device_with_vid_pid(VID, PID)?;
    let _ = handle.set_auto_detach_kernel_driver(true);
    handle.claim_interface(IFACE).ok()?;
    Some(handle)
}

/// Send GET_HW_STATUS and decode the response.
pub(crate) fn poll_status(handle: &rusb::DeviceHandle<rusb::Context>) -> Option<State> {
    let cmd = envelope(&GHS_CDB);

    // Phase 1: command
    handle.write_bulk(EP_OUT, &cmd, USB_TIMEOUT).ok()?;

    // Phase 2: data (12 bytes of hardware status)
    let mut buf = [0u8; 64];
    let n = handle.read_bulk(EP_IN, &mut buf, USB_TIMEOUT).ok()?;

    // Phase 3: drain the status envelope (0x53...)
    let mut discard = [0u8; 64];
    let _ = handle.read_bulk(EP_IN, &mut discard, STATUS_TIMEOUT);

    debug!(
        "raw: {}",
        buf[..n]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    );

    Some(State::from_response(&buf[..n]))
}

/// Release the USB handle so another process (scanimage) can claim the device.
fn release_usb(handle: rusb::DeviceHandle<rusb::Context>) {
    let _ = handle.release_interface(IFACE);
    drop(handle);
    debug!("usb: released for handler");
}

// ── Event dispatch ───────────────────────────────────────────────────

/// Run the handler script with the given arguments, synchronously.
fn run_handler(script: &str, args: &[&str]) {
    debug!("exec: {script} {}", args.join(" "));
    match ShellCommand::new(script).args(args).status() {
        Ok(s) if s.success() => debug!("handler ok"),
        Ok(s) => warn!("handler exited: {s}"),
        Err(e) => error!("handler failed: {e}"),
    }
}

// ── Operating modes ──────────────────────────────────────────────────

/// What mode the daemon is running in.
#[allow(clippy::enum_variant_names)]
enum Mode {
    /// Log events only, no handler.
    LogOnly,
    /// Legacy: fire handler with raw event names (no gesture detection).
    Legacy(String),
    /// Config: gesture detection on button, handler with profile dispatch.
    ConfigMode(Config),
}

// ── Main loop ────────────────────────────────────────────────────────

fn print_usage() {
    eprintln!(
        "s1500d — event daemon for the Fujitsu ScanSnap S1500\n\
         \n\
         Usage:\n\
         \x20 s1500d                   Monitor and log events\n\
         \x20 s1500d HANDLER           Run HANDLER on each raw event\n\
         \x20 s1500d -c CONFIG.toml    Gesture detection + profile dispatch\n\
         \x20 s1500d --doctor          Interactive hardware verification\n\
         \x20 s1500d --help            Show this message\n\
         \n\
         Handler mode (s1500d HANDLER) — handler receives the event name as $1:\n\
         \x20 device-arrived   Scanner lid opened (USB device appeared)\n\
         \x20 device-left      Scanner lid closed (USB device removed)\n\
         \x20 paper-in         Paper inserted into feeder\n\
         \x20 paper-out        Paper removed from feeder\n\
         \x20 button-down      Scan button pressed\n\
         \x20 button-up        Scan button released\n\
         \n\
         Config mode (s1500d -c CONFIG.toml) — handler receives:\n\
         \x20 scan <profile>   Gesture completed (press count mapped to profile)\n\
         \x20 paper-in         Paper inserted (no second arg)\n\
         \x20 paper-out        Paper removed (no second arg)\n\
         \x20 device-arrived   Scanner appeared (no second arg)\n\
         \x20 device-left      Scanner removed (no second arg)\n\
         \n\
         Set log_level = \"debug\" in config.toml for verbose output\n\
         (or RUST_LOG=debug to override)."
    );
}

/// What action the event loop should take after processing transitions.
#[derive(Debug)]
enum Action {
    /// No handler to run — just continue polling.
    Continue,
    /// Run handler with USB release/reclaim. Args: (script, args).
    RunHandler(String, Vec<String>),
}

fn run(mode: Mode) -> ! {
    let ctx = rusb::Context::new().expect("failed to create USB context");
    let mut was_present = false;
    let mut prev: Option<State> = None;
    let mut gesture = GestureState::Idle;

    loop {
        // ── Phase 1: wait for device ─────────────────────────────
        let mut handle = loop {
            match try_open(&ctx) {
                Some(h) => break h,
                None => {
                    if was_present {
                        info!("{}", Event::DeviceLeft.tag());
                        emit_handler(&mode, &[Event::DeviceLeft.tag()]);
                        was_present = false;
                        prev = None;
                        gesture = GestureState::Idle;
                    }
                    thread::sleep(RECONNECT_INTERVAL);
                }
            }
        };

        if !was_present {
            info!("{}", Event::DeviceArrived.tag());
            emit_handler(&mode, &[Event::DeviceArrived.tag()]);
            was_present = true;
        }

        // ── Phase 2: poll status while device is alive ───────────
        'poll: loop {
            // Check gesture timeout before polling
            let gesture_action = check_gesture_timeout(&gesture, &mode);
            if let Some(action) = gesture_action {
                gesture = GestureState::Idle;
                match action {
                    Action::Continue => {}
                    Action::RunHandler(script, args) => {
                        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        release_usb(handle);
                        run_handler(&script, &arg_refs);
                        match try_open(&ctx) {
                            Some(h) => {
                                handle = h;
                                if let Some(fresh) = poll_status(&handle) {
                                    prev = Some(fresh);
                                } else {
                                    break 'poll;
                                }
                            }
                            None => {
                                debug!("usb: reclaim failed after handler, device gone");
                                break 'poll;
                            }
                        }
                    }
                }
            }

            let Some(state) = poll_status(&handle) else {
                // USB error — device likely disconnected.
                debug!("poll failed, assuming device left");
                break;
            };

            match prev {
                None => {
                    info!("initial: paper={} button={}", state.paper, state.button);
                }
                Some(p) => {
                    // Determine what action to take based on transitions.
                    // We process events to decide on a single action, then execute it.
                    let action = process_transitions(p, state, &mode, &mut gesture);

                    match action {
                        Action::Continue => {
                            // No handler ran. prev = Some(state) at the bottom
                            // of the loop updates the baseline naturally.
                            // Do NOT re-read here — it would swallow the ButtonUp
                            // transition from momentary 0x01 taps.
                        }
                        Action::RunHandler(script, args) => {
                            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                            release_usb(handle);
                            run_handler(&script, &arg_refs);
                            match try_open(&ctx) {
                                Some(h) => {
                                    handle = h;
                                    if let Some(fresh) = poll_status(&handle) {
                                        prev = Some(fresh);
                                        thread::sleep(POLL_INTERVAL);
                                        continue 'poll;
                                    } else {
                                        break 'poll;
                                    }
                                }
                                None => {
                                    debug!("usb: reclaim failed, device gone");
                                    break 'poll;
                                }
                            }
                        }
                    }
                }
            }

            prev = Some(state);

            // In config mode with a pending gesture, poll faster to hit timeout promptly
            let sleep = match (&mode, &gesture) {
                (Mode::ConfigMode(_), GestureState::Released(_, _)) => Duration::from_millis(20),
                _ => POLL_INTERVAL,
            };
            thread::sleep(sleep);
        }
    }
}

/// Check if a gesture timeout has expired and return the action to take.
fn check_gesture_timeout(gesture: &GestureState, mode: &Mode) -> Option<Action> {
    let config = match mode {
        Mode::ConfigMode(c) => c,
        _ => return None,
    };
    let (count, ts) = match gesture {
        GestureState::Released(count, ts) => (*count, *ts),
        _ => return None,
    };
    if ts.elapsed() < config.gesture_timeout() {
        return None;
    }

    if let Some(profile) = config.profiles.get(&count) {
        info!("scan {} ({}x press)", profile, count);
        Some(Action::RunHandler(
            config.handler.clone(),
            vec!["scan".into(), profile.clone()],
        ))
    } else {
        info!("{}x press — no profile mapped, ignoring", count);
        Some(Action::Continue)
    }
}

/// Process state transitions and return what action to take.
///
/// For config mode, button events update the gesture state machine (no handler yet).
/// For legacy mode, the first event triggers handler dispatch.
/// For log-only, events are logged and Action::Continue is returned.
fn process_transitions(
    prev: State,
    curr: State,
    mode: &Mode,
    gesture: &mut GestureState,
) -> Action {
    for ev in transitions(prev, curr) {
        match mode {
            Mode::ConfigMode(ref config) => {
                match ev {
                    Event::ButtonDown => {
                        *gesture = match *gesture {
                            GestureState::Idle => {
                                debug!("gesture: press 1");
                                GestureState::Pressed(1)
                            }
                            GestureState::Released(n, _) => {
                                debug!("gesture: press {}", n + 1);
                                GestureState::Pressed(n + 1)
                            }
                            // Shouldn't happen (double down without up)
                            GestureState::Pressed(n) => GestureState::Pressed(n),
                        };
                    }
                    Event::ButtonUp => {
                        *gesture = match *gesture {
                            GestureState::Pressed(n) => {
                                debug!("gesture: release {n}, waiting...");
                                GestureState::Released(n, Instant::now())
                            }
                            _ => GestureState::Idle,
                        };
                    }
                    // Non-button events: fire handler immediately
                    _ => {
                        info!("{}", ev.tag());
                        return Action::RunHandler(config.handler.clone(), vec![ev.tag().into()]);
                    }
                }
            }
            Mode::Legacy(ref script) => {
                info!("{}", ev.tag());
                return Action::RunHandler(script.clone(), vec![ev.tag().into()]);
            }
            Mode::LogOnly => {
                info!("{}", ev.tag());
            }
        }
    }
    Action::Continue
}

/// Run the handler for lifecycle events (device-arrived/left) that don't need USB release.
fn emit_handler(mode: &Mode, args: &[&str]) {
    match mode {
        Mode::LogOnly => {}
        Mode::Legacy(script) => run_handler(script, args),
        Mode::ConfigMode(config) => run_handler(&config.handler, args),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Handle --help/--doctor before logger init (they don't need it).
    match args.get(1).map(String::as_str) {
        Some("--help" | "-h") => {
            print_usage();
            std::process::exit(0);
        }
        Some("--doctor") => {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
                .format_timestamp_secs()
                .init();
            doctor();
            return;
        }
        _ => {}
    }

    // In config mode, load config first so log_level can feed the logger.
    let config = if args.get(1).map(String::as_str) == Some("-c") {
        let config_path = args.get(2).unwrap_or_else(|| {
            eprintln!("s1500d: -c requires a config file path");
            std::process::exit(1);
        });
        Some(load_config(config_path))
    } else {
        None
    };

    // Use config log_level as default filter if RUST_LOG is not set.
    let default_filter = if std::env::var("RUST_LOG").is_ok() {
        "info" // env var takes priority; from_env will use it regardless of this fallback
    } else {
        config.as_ref().map_or("info", |c| c.log_level.as_str())
    };

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_filter))
        .format_timestamp_secs()
        .init();

    match args.get(1).map(String::as_str) {
        Some("-c") => {
            let config = config.unwrap();
            let config_path = args.get(2).unwrap();
            info!(
                "s1500d starting — config: {config_path}, handler: {}, profiles: {:?}",
                config.handler, config.profiles
            );
            run(Mode::ConfigMode(config));
        }
        Some(h) => {
            info!("s1500d starting — handler: {h} (legacy mode)");
            run(Mode::Legacy(h.to_string()));
        }
        None => {
            info!("s1500d starting — no handler (log only)");
            run(Mode::LogOnly);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── State::from_response ─────────────────────────────────────

    #[test]
    fn state_idle_scanner() {
        // byte 3 = 0x80 (hopper empty), byte 4 = 0x00 (button not pressed)
        let buf = [0, 0, 0, 0x80, 0x00, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf);
        assert!(!s.paper);
        assert!(!s.button);
    }

    #[test]
    fn state_paper_present() {
        // byte 3 = 0x00 (bit 7 clear = paper present)
        let buf = [0, 0, 0, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf);
        assert!(s.paper);
        assert!(!s.button);
    }

    #[test]
    fn state_button_held() {
        // byte 4 = 0x20 (bit 5 = button held)
        let buf = [0, 0, 0, 0x80, 0x20, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf);
        assert!(!s.paper);
        assert!(s.button);
    }

    #[test]
    fn state_button_momentary_tap() {
        // byte 4 = 0x01 (bit 0 = momentary tap)
        let buf = [0, 0, 0, 0x80, 0x01, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf);
        assert!(s.button);
    }

    #[test]
    fn state_button_both_bits() {
        // byte 4 = 0x21 (both button bits set)
        let buf = [0, 0, 0, 0x80, 0x21, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf);
        assert!(s.button);
    }

    #[test]
    fn state_paper_and_button() {
        // byte 3 = 0x00 (paper present), byte 4 = 0x20 (button held)
        let buf = [0, 0, 0, 0x00, 0x20, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf);
        assert!(s.paper);
        assert!(s.button);
    }

    #[test]
    fn state_short_buffer() {
        // Too short for byte 3 or 4 — should default to false
        let s = State::from_response(&[0, 0]);
        assert!(!s.paper);
        assert!(!s.button);
    }

    #[test]
    fn state_empty_buffer() {
        let s = State::from_response(&[]);
        assert!(!s.paper);
        assert!(!s.button);
    }

    #[test]
    fn state_other_bits_ignored() {
        // byte 3 has non-0x80 bits set but bit 7 is set → no paper
        let buf = [0, 0, 0, 0xFF, 0x00, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf);
        assert!(!s.paper);

        // byte 4 has bits set but not 0x20 or 0x01 → no button
        let buf = [0, 0, 0, 0x80, 0xDE, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf);
        assert!(!s.button);
    }

    // ── envelope ─────────────────────────────────────────────────

    #[test]
    fn envelope_wraps_cdb() {
        let cdb = [0xC2, 0, 0, 0, 0, 0, 0, 0, 0x0C, 0];
        let env = envelope(&cdb);
        assert_eq!(env[0], 0x43);
        assert_eq!(&env[1..19], &[0u8; 18]);
        assert_eq!(&env[19..29], &cdb);
        assert_eq!(&env[29..31], &[0, 0]);
    }

    #[test]
    fn envelope_short_cdb() {
        let cdb = [0xAA];
        let env = envelope(&cdb);
        assert_eq!(env[0], 0x43);
        assert_eq!(env[19], 0xAA);
        assert_eq!(&env[20..31], &[0u8; 11]);
    }

    // ── transitions ──────────────────────────────────────────────

    #[test]
    fn transitions_no_change() {
        let s = State {
            paper: false,
            button: false,
        };
        let events: Vec<_> = transitions(s, s).collect();
        assert!(events.is_empty());
    }

    #[test]
    fn transitions_paper_in() {
        let prev = State {
            paper: false,
            button: false,
        };
        let curr = State {
            paper: true,
            button: false,
        };
        let events: Vec<_> = transitions(prev, curr).collect();
        assert_eq!(events, vec![Event::PaperIn]);
    }

    #[test]
    fn transitions_paper_out() {
        let prev = State {
            paper: true,
            button: false,
        };
        let curr = State {
            paper: false,
            button: false,
        };
        let events: Vec<_> = transitions(prev, curr).collect();
        assert_eq!(events, vec![Event::PaperOut]);
    }

    #[test]
    fn transitions_button_down() {
        let prev = State {
            paper: false,
            button: false,
        };
        let curr = State {
            paper: false,
            button: true,
        };
        let events: Vec<_> = transitions(prev, curr).collect();
        assert_eq!(events, vec![Event::ButtonDown]);
    }

    #[test]
    fn transitions_button_up() {
        let prev = State {
            paper: false,
            button: true,
        };
        let curr = State {
            paper: false,
            button: false,
        };
        let events: Vec<_> = transitions(prev, curr).collect();
        assert_eq!(events, vec![Event::ButtonUp]);
    }

    #[test]
    fn transitions_simultaneous() {
        let prev = State {
            paper: false,
            button: false,
        };
        let curr = State {
            paper: true,
            button: true,
        };
        let events: Vec<_> = transitions(prev, curr).collect();
        assert_eq!(events, vec![Event::PaperIn, Event::ButtonDown]);
    }

    // ── event tags ───────────────────────────────────────────────

    #[test]
    fn event_tags() {
        assert_eq!(Event::DeviceArrived.tag(), "device-arrived");
        assert_eq!(Event::DeviceLeft.tag(), "device-left");
        assert_eq!(Event::PaperIn.tag(), "paper-in");
        assert_eq!(Event::PaperOut.tag(), "paper-out");
        assert_eq!(Event::ButtonDown.tag(), "button-down");
        assert_eq!(Event::ButtonUp.tag(), "button-up");
    }

    // ── process_transitions ──────────────────────────────────────

    fn test_config() -> Config {
        Config {
            handler: "/bin/test-handler.sh".into(),
            gesture_timeout_ms: 400,
            log_level: "info".into(),
            profiles: HashMap::from([(1, "standard".into()), (2, "legal".into())]),
        }
    }

    #[test]
    fn process_log_only_returns_continue() {
        let prev = State {
            paper: false,
            button: false,
        };
        let curr = State {
            paper: true,
            button: false,
        };
        let mut gesture = GestureState::Idle;
        let action = process_transitions(prev, curr, &Mode::LogOnly, &mut gesture);
        assert!(matches!(action, Action::Continue));
    }

    #[test]
    fn process_legacy_fires_handler() {
        let prev = State {
            paper: false,
            button: false,
        };
        let curr = State {
            paper: true,
            button: false,
        };
        let mut gesture = GestureState::Idle;
        let mode = Mode::Legacy("/bin/handler.sh".into());
        let action = process_transitions(prev, curr, &mode, &mut gesture);
        match action {
            Action::RunHandler(script, args) => {
                assert_eq!(script, "/bin/handler.sh");
                assert_eq!(args, vec!["paper-in"]);
            }
            Action::Continue => panic!("expected RunHandler"),
        }
    }

    #[test]
    fn process_config_button_down_starts_gesture() {
        let prev = State {
            paper: false,
            button: false,
        };
        let curr = State {
            paper: false,
            button: true,
        };
        let mut gesture = GestureState::Idle;
        let mode = Mode::ConfigMode(test_config());
        let action = process_transitions(prev, curr, &mode, &mut gesture);
        assert!(matches!(action, Action::Continue));
        assert!(matches!(gesture, GestureState::Pressed(1)));
    }

    #[test]
    fn process_config_button_up_releases_gesture() {
        let prev = State {
            paper: false,
            button: true,
        };
        let curr = State {
            paper: false,
            button: false,
        };
        let mut gesture = GestureState::Pressed(1);
        let mode = Mode::ConfigMode(test_config());
        let action = process_transitions(prev, curr, &mode, &mut gesture);
        assert!(matches!(action, Action::Continue));
        assert!(matches!(gesture, GestureState::Released(1, _)));
    }

    #[test]
    fn process_config_double_press() {
        let mut gesture = GestureState::Released(1, Instant::now());
        let mode = Mode::ConfigMode(test_config());

        // Second button down
        let prev = State {
            paper: false,
            button: false,
        };
        let curr = State {
            paper: false,
            button: true,
        };
        let action = process_transitions(prev, curr, &mode, &mut gesture);
        assert!(matches!(action, Action::Continue));
        assert!(matches!(gesture, GestureState::Pressed(2)));
    }

    #[test]
    fn process_config_paper_fires_immediately() {
        let prev = State {
            paper: false,
            button: false,
        };
        let curr = State {
            paper: true,
            button: false,
        };
        let mut gesture = GestureState::Idle;
        let mode = Mode::ConfigMode(test_config());
        let action = process_transitions(prev, curr, &mode, &mut gesture);
        match action {
            Action::RunHandler(script, args) => {
                assert_eq!(script, "/bin/test-handler.sh");
                assert_eq!(args, vec!["paper-in"]);
            }
            Action::Continue => panic!("expected RunHandler for paper-in"),
        }
    }

    #[test]
    fn process_no_change_returns_continue() {
        let s = State {
            paper: false,
            button: false,
        };
        let mut gesture = GestureState::Idle;
        let action = process_transitions(s, s, &Mode::LogOnly, &mut gesture);
        assert!(matches!(action, Action::Continue));
    }

    // ── check_gesture_timeout ────────────────────────────────────

    #[test]
    fn gesture_timeout_not_config_mode() {
        let gesture = GestureState::Released(1, Instant::now());
        let mode = Mode::LogOnly;
        assert!(check_gesture_timeout(&gesture, &mode).is_none());
    }

    #[test]
    fn gesture_timeout_not_released() {
        let gesture = GestureState::Pressed(1);
        let mode = Mode::ConfigMode(test_config());
        assert!(check_gesture_timeout(&gesture, &mode).is_none());
    }

    #[test]
    fn gesture_timeout_not_expired() {
        let gesture = GestureState::Released(1, Instant::now());
        let mode = Mode::ConfigMode(test_config());
        assert!(check_gesture_timeout(&gesture, &mode).is_none());
    }

    #[test]
    fn gesture_timeout_expired_mapped() {
        // Use a timestamp far enough in the past
        let gesture = GestureState::Released(1, Instant::now() - Duration::from_secs(1));
        let mode = Mode::ConfigMode(test_config());
        let action = check_gesture_timeout(&gesture, &mode);
        match action {
            Some(Action::RunHandler(script, args)) => {
                assert_eq!(script, "/bin/test-handler.sh");
                assert_eq!(args, vec!["scan", "standard"]);
            }
            other => panic!("expected RunHandler, got {other:?}"),
        }
    }

    #[test]
    fn gesture_timeout_expired_double_press() {
        let gesture = GestureState::Released(2, Instant::now() - Duration::from_secs(1));
        let mode = Mode::ConfigMode(test_config());
        let action = check_gesture_timeout(&gesture, &mode);
        match action {
            Some(Action::RunHandler(_, args)) => {
                assert_eq!(args, vec!["scan", "legal"]);
            }
            other => panic!("expected RunHandler for double press, got {other:?}"),
        }
    }

    #[test]
    fn gesture_timeout_expired_unmapped() {
        let gesture = GestureState::Released(5, Instant::now() - Duration::from_secs(1));
        let mode = Mode::ConfigMode(test_config());
        let action = check_gesture_timeout(&gesture, &mode);
        assert!(matches!(action, Some(Action::Continue)));
    }
}
