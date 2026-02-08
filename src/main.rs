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

use std::collections::HashMap;
use std::io::{self, BufRead, Write as IoWrite};
use std::process::Command as ShellCommand;
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use rusb::UsbContext;
use serde::Deserialize;

// ── Device constants ──────────────────────────────────────────────────

const VID: u16 = 0x04C5;
const PID: u16 = 0x11A2;
const EP_OUT: u8 = 0x02;
const EP_IN: u8 = 0x81;
const IFACE: u8 = 0;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
const USB_TIMEOUT: Duration = Duration::from_millis(1000);
const STATUS_TIMEOUT: Duration = Duration::from_millis(200);

// ── Config ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawConfig {
    handler: String,
    #[serde(default = "default_gesture_timeout_ms")]
    gesture_timeout_ms: u64,
    #[serde(default)]
    profiles: HashMap<String, String>,
}

fn default_gesture_timeout_ms() -> u64 {
    400
}

#[derive(Debug)]
struct Config {
    handler: String,
    gesture_timeout_ms: u64,
    profiles: HashMap<u32, String>,
}

impl Config {
    fn gesture_timeout(&self) -> Duration {
        Duration::from_millis(self.gesture_timeout_ms)
    }
}

fn load_config(path: &str) -> Config {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("s1500d: cannot read config {path}: {e}");
        std::process::exit(1);
    });
    let raw: RawConfig = toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("s1500d: invalid config {path}: {e}");
        std::process::exit(1);
    });
    let profiles: HashMap<u32, String> = raw
        .profiles
        .into_iter()
        .map(|(k, v)| {
            let n: u32 = k.parse().unwrap_or_else(|_| {
                eprintln!("s1500d: profile key {k:?} is not a valid press count");
                std::process::exit(1);
            });
            (n, v)
        })
        .collect();
    Config {
        handler: raw.handler,
        gesture_timeout_ms: raw.gesture_timeout_ms,
        profiles,
    }
}

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
struct State {
    paper: bool,  // paper present in hopper
    button: bool, // scan button physically held down
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
fn try_open(ctx: &rusb::Context) -> Option<rusb::DeviceHandle<rusb::Context>> {
    let handle = ctx.open_device_with_vid_pid(VID, PID)?;
    let _ = handle.set_auto_detach_kernel_driver(true);
    handle.claim_interface(IFACE).ok()?;
    Some(handle)
}

/// Send GET_HW_STATUS and decode the response.
fn poll_status(handle: &rusb::DeviceHandle<rusb::Context>) -> Option<State> {
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
         \x20 s1500d HANDLER           Legacy: run HANDLER on each raw event\n\
         \x20 s1500d -c CONFIG.toml    Config: gesture detection + profiles\n\
         \x20 s1500d --doctor          Interactive hardware verification\n\
         \x20 s1500d --help            Show this message\n\
         \n\
         Legacy mode — handler receives the event name as $1:\n\
         \x20 device-arrived   Scanner lid opened (USB device appeared)\n\
         \x20 device-left      Scanner lid closed (USB device removed)\n\
         \x20 paper-in         Paper inserted into feeder\n\
         \x20 paper-out        Paper removed from feeder\n\
         \x20 button-down      Scan button pressed\n\
         \x20 button-up        Scan button released\n\
         \n\
         Config mode — handler receives:\n\
         \x20 scan <profile>   Gesture completed (press count mapped to profile)\n\
         \x20 paper-in         Paper inserted (no second arg)\n\
         \x20 paper-out        Paper removed (no second arg)\n\
         \x20 device-arrived   Scanner appeared (no second arg)\n\
         \x20 device-left      Scanner removed (no second arg)\n\
         \n\
         Set RUST_LOG=debug for verbose output."
    );
}

/// What action the event loop should take after processing transitions.
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

// ── Doctor mode ──────────────────────────────────────────────────────

const DOCTOR_TIMEOUT: Duration = Duration::from_secs(15);

/// Block until the user presses Enter.
fn wait_enter() {
    let _ = io::stdout().flush();
    let _ = io::stdin().lock().read_line(&mut String::new());
}

/// Poll until `predicate` is satisfied or `timeout` elapses.
/// Prints dots to show progress. Returns the matching state or None.
fn wait_for_state(
    handle: &rusb::DeviceHandle<rusb::Context>,
    predicate: impl Fn(&State) -> bool,
    timeout: Duration,
) -> Option<State> {
    let start = Instant::now();
    let mut dots = 0u32;
    print!("      Polling");
    let _ = io::stdout().flush();
    loop {
        if let Some(state) = poll_status(handle) {
            if predicate(&state) {
                return Some(state);
            }
        }
        if start.elapsed() >= timeout {
            return None;
        }
        // Print a dot every 500ms
        let expected = (start.elapsed().as_millis() / 500) as u32;
        if dots < expected {
            print!(".");
            let _ = io::stdout().flush();
            dots = expected;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn doctor() {
    println!("s1500d doctor");
    println!("=============\n");
    println!("Verifying USB communication and hardware event detection");
    println!("for the Fujitsu ScanSnap S1500.\n");

    let ctx = match rusb::Context::new() {
        Ok(c) => c,
        Err(e) => {
            println!("[1/6] USB context ............. FAIL ({e})");
            println!("\n      Cannot initialize libusb. Is it installed?");
            std::process::exit(1);
        }
    };

    // ── 1. USB connection ────────────────────────────────────────
    print!("[1/6] USB connection .......... ");
    let _ = io::stdout().flush();
    let handle = match try_open(&ctx) {
        Some(h) => {
            println!("ok");
            h
        }
        None => {
            println!("FAIL");
            println!("\n      Scanner not found (04c5:11a2).");
            println!("      Is the ADF lid open? Check: lsusb | grep 04c5");
            std::process::exit(1);
        }
    };

    // ── 2. GET_HW_STATUS ─────────────────────────────────────────
    print!("[2/6] Hardware status ......... ");
    let _ = io::stdout().flush();
    let baseline = match poll_status(&handle) {
        Some(s) => {
            println!("ok  (paper={}, button={})", s.paper, s.button);
            s
        }
        None => {
            println!("FAIL");
            println!("\n      GET_HW_STATUS returned no data. USB communication error.");
            std::process::exit(1);
        }
    };

    let mut passed = 2u32;
    let mut failed = 0u32;

    // ── 3. Paper detect ──────────────────────────────────────────
    println!("\n[3/6] Paper detect");
    if baseline.paper {
        print!("      Paper already in feeder — remove it first, then press Enter: ");
        wait_enter();
        if wait_for_state(&handle, |s| !s.paper, DOCTOR_TIMEOUT).is_none() {
            println!(" timed out — could not establish empty baseline");
        }
        println!();
    }
    print!("      Press Enter, then insert a sheet of paper: ");
    wait_enter();
    match wait_for_state(&handle, |s| s.paper, DOCTOR_TIMEOUT) {
        Some(_) => {
            println!(" detected!       PASS");
            passed += 1;
        }
        None => {
            println!(" timed out       FAIL");
            failed += 1;
        }
    }

    // ── 4. Paper remove ──────────────────────────────────────────
    println!("\n[4/6] Paper remove");
    print!("      Press Enter, then remove the paper: ");
    wait_enter();
    match wait_for_state(&handle, |s| !s.paper, DOCTOR_TIMEOUT) {
        Some(_) => {
            println!(" detected!       PASS");
            passed += 1;
        }
        None => {
            println!(" timed out       FAIL");
            failed += 1;
        }
    }

    // ── 5. Button press ──────────────────────────────────────────
    println!("\n[5/6] Button press");
    if baseline.button {
        print!("      Button appears held — release it first, then press Enter: ");
        wait_enter();
        let _ = wait_for_state(&handle, |s| !s.button, DOCTOR_TIMEOUT);
        println!();
    }
    print!("      Press Enter, then press and HOLD the scan button: ");
    wait_enter();
    match wait_for_state(&handle, |s| s.button, DOCTOR_TIMEOUT) {
        Some(_) => {
            println!(" detected!       PASS");
            passed += 1;
        }
        None => {
            println!(" timed out       FAIL");
            failed += 1;
        }
    }

    // ── 6. Button release ────────────────────────────────────────
    println!("\n[6/6] Button release");
    println!("      Release the button now.");
    match wait_for_state(&handle, |s| !s.button, DOCTOR_TIMEOUT) {
        Some(_) => {
            println!(" detected!       PASS");
            passed += 1;
        }
        None => {
            println!(" timed out       FAIL");
            failed += 1;
        }
    }

    // ── Summary ──────────────────────────────────────────────────
    let total = passed + failed;
    println!("\n=============");
    if failed == 0 {
        println!("All {total} checks passed. Scanner is working correctly.");
    } else {
        println!("{passed}/{total} passed, {failed} failed.");
        std::process::exit(1);
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("--help" | "-h") => {
            print_usage();
            std::process::exit(0);
        }
        Some("--doctor") => {
            doctor();
        }
        Some("-c") => {
            let config_path = args.get(2).unwrap_or_else(|| {
                eprintln!("s1500d: -c requires a config file path");
                std::process::exit(1);
            });
            let config = load_config(config_path);
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
