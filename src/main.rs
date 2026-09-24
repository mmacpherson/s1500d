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
#[cfg(test)]
mod sim;

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
const MAX_POLL_FAILURES: u32 = 3;

// ── Fujitsu USB protocol ─────────────────────────────────────────────

/// Wrap a SCSI CDB in the 31-byte Fujitsu USB command envelope.
fn envelope(cdb: &[u8]) -> [u8; 31] {
    debug_assert!(cdb.len() <= 12, "CDB exceeds 12-byte envelope capacity");
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
    fn from_response(buf: &[u8]) -> Option<Self> {
        if buf.len() != HW_STATUS_LEN {
            return None;
        }
        Some(Self {
            paper: buf[3] & 0x80 == 0,
            // bit 5 (0x20) = button held; bit 0 (0x01) = button momentary/tap
            button: buf[4] & 0x21 != 0,
        })
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
///
/// Time is observation time: a release first seen after a handler returns is
/// stamped when it was seen. A press observed after the window has already
/// expired finishes the old gesture and starts a new one.
#[derive(Debug, Clone, Copy)]
enum GestureState {
    Idle,
    Pressed(u32),
    Released(u32, Instant),
}

// ── Hardware seams ───────────────────────────────────────────────────

/// Bulk-transfer access to a claimed scanner interface.
pub(crate) trait Link {
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> rusb::Result<usize>;
    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> rusb::Result<usize>;
}

impl Link for rusb::DeviceHandle<rusb::Context> {
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> rusb::Result<usize> {
        rusb::DeviceHandle::write_bulk(self, endpoint, buf, timeout)
    }

    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> rusb::Result<usize> {
        rusb::DeviceHandle::read_bulk(self, endpoint, buf, timeout)
    }
}

/// Everything the event loop needs from outside the process: the device,
/// handler execution, and time. Tests substitute a scripted implementation.
trait Host {
    type Handle: Link;

    /// Open and claim the scanner.
    fn open(&mut self) -> Option<Self::Handle>;
    /// Open, reset, and reopen the scanner to clear stale protocol state.
    fn open_with_reset(&mut self) -> Option<Self::Handle>;
    /// Reset a wedged device and reopen it (unverified).
    fn reset(&mut self, handle: Self::Handle) -> Option<Self::Handle>;
    /// Release the interface so another process can claim the device.
    fn release(&mut self, handle: Self::Handle);
    /// Run the handler script synchronously.
    fn run_handler(&mut self, script: &str, args: &[&str]);
    fn sleep(&mut self, duration: Duration);
    fn now(&self) -> Instant;
    /// Whether the event loop should continue. Always true outside tests.
    fn keep_running(&mut self) -> bool {
        true
    }
}

// ── USB communication ────────────────────────────────────────────────

/// Open the scanner, returning a claimed device handle.
pub(crate) fn try_open(ctx: &rusb::Context) -> Option<rusb::DeviceHandle<rusb::Context>> {
    let handle = ctx.open_device_with_vid_pid(VID, PID)?;
    let _ = handle.set_auto_detach_kernel_driver(true);
    handle.claim_interface(IFACE).ok()?;
    Some(handle)
}

/// The real host: libusb, child processes, and wall-clock time.
struct UsbHost {
    ctx: rusb::Context,
}

impl Host for UsbHost {
    type Handle = rusb::DeviceHandle<rusb::Context>;

    fn open(&mut self) -> Option<Self::Handle> {
        try_open(&self.ctx)
    }

    /// Used in the outer reconnect loop to ensure a clean connection after a
    /// previous s1500d process may have left the device in a bad state (e.g.,
    /// after `systemctl restart`).
    fn open_with_reset(&mut self) -> Option<Self::Handle> {
        let handle = try_open(&self.ctx)?;
        info!("usb: resetting device for clean state");
        if handle.reset().is_err() {
            warn!("usb: reset failed, proceeding with existing handle");
            return Some(handle);
        }
        // Drop stale handle, wait for device to re-enumerate, then re-open fresh.
        drop(handle);
        thread::sleep(Duration::from_millis(200));
        try_open(&self.ctx)
    }

    /// Takes ownership of the stale handle (preventing accidental reuse),
    /// resets, drops, and re-opens.
    fn reset(&mut self, handle: Self::Handle) -> Option<Self::Handle> {
        let _ = handle.reset();
        drop(handle);
        thread::sleep(Duration::from_millis(200));
        try_open(&self.ctx)
    }

    fn release(&mut self, handle: Self::Handle) {
        let _ = handle.release_interface(IFACE);
        drop(handle);
        debug!("usb: released for handler");
    }

    fn run_handler(&mut self, script: &str, args: &[&str]) {
        run_handler(script, args);
    }

    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Attempt to recover from consecutive poll failures by resetting the device,
/// then verify responsiveness with a test poll. Returns the verification
/// sample too: it is a real observation, not to be discarded.
fn try_reset_device<H: Host>(host: &mut H, handle: H::Handle) -> Option<(H::Handle, State)> {
    info!("usb: poll failures hit threshold, attempting device reset");
    let new_handle = host.reset(handle)?;
    match poll_status(&new_handle) {
        Ok(state) => {
            info!("usb: device reset successful, resuming");
            Some((new_handle, state))
        }
        Err(e) => {
            warn!("usb: device unresponsive after reset: {e}");
            None
        }
    }
}

/// Length of the GET_HW_STATUS data phase (the CDB's allocation length).
const HW_STATUS_LEN: usize = 12;
/// Fujitsu USB status envelope: 13 bytes, code 0x53, SCSI status at byte 9.
const USB_STATUS_CODE: u8 = 0x53;
const USB_STATUS_LEN: usize = 13;
const USB_STATUS_OFFSET: usize = 9;

/// Why a GET_HW_STATUS transaction was rejected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PollError {
    Write(rusb::Error),
    ShortWrite(usize),
    ReadData(rusb::Error),
    /// A status envelope arrived where sensor data was expected.
    StatusInData(Vec<u8>),
    BadDataLength(usize),
    ReadStatus(rusb::Error),
    BadStatus(Vec<u8>),
}

impl std::fmt::Display for PollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Write(e) => write!(f, "command write failed: {e}"),
            Self::ShortWrite(n) => write!(f, "short command write: {n}/31 bytes"),
            Self::ReadData(e) => write!(f, "data read failed: {e}"),
            Self::StatusInData(b) => write!(f, "status envelope in data phase: {}", hex(b)),
            Self::BadDataLength(n) => {
                write!(f, "data phase returned {n} bytes (want {HW_STATUS_LEN})")
            }
            Self::ReadStatus(e) => write!(f, "status read failed: {e}"),
            Self::BadStatus(b) => write!(f, "bad status envelope: {}", hex(b)),
        }
    }
}

fn hex(buf: &[u8]) -> String {
    buf.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_good_status(buf: &[u8]) -> bool {
    buf.len() == USB_STATUS_LEN && buf[0] == USB_STATUS_CODE && buf[USB_STATUS_OFFSET] == 0
}

/// Send GET_HW_STATUS and decode the response. The transaction counts only if
/// every phase completes with the documented shape.
pub(crate) fn poll_status(link: &impl Link) -> Result<State, PollError> {
    let cmd = envelope(&GHS_CDB);

    // Phase 1: command
    let n = link
        .write_bulk(EP_OUT, &cmd, USB_TIMEOUT)
        .map_err(PollError::Write)?;
    if n != cmd.len() {
        return Err(PollError::ShortWrite(n));
    }

    // Phase 2: data (12 bytes of hardware status)
    let mut buf = [0u8; 64];
    let n = link
        .read_bulk(EP_IN, &mut buf, USB_TIMEOUT)
        .map_err(PollError::ReadData)?;
    let data = &buf[..n];
    debug!("raw: {}", hex(data));

    // A command the device rejected answers with status and no data; there
    // is no further phase to drain.
    if n == USB_STATUS_LEN && data[0] == USB_STATUS_CODE {
        return Err(PollError::StatusInData(data.to_vec()));
    }

    // Phase 3: status envelope (0x53...). Read it even if the data is bad so
    // the next transaction starts in sync.
    let mut status = [0u8; 64];
    let m = link
        .read_bulk(EP_IN, &mut status, STATUS_TIMEOUT)
        .map_err(PollError::ReadStatus)?;
    debug!("status: {}", hex(&status[..m]));
    if !is_good_status(&status[..m]) {
        return Err(PollError::BadStatus(status[..m].to_vec()));
    }

    State::from_response(data).ok_or(PollError::BadDataLength(n))
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
         \x20 s1500d --version         Show version\n\
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

/// Handler invocations observed but not yet run, oldest first. Each entry is
/// the handler's argument list.
type Dispatches = Vec<Vec<String>>;

impl Mode {
    fn handler(&self) -> Option<&str> {
        match self {
            Mode::LogOnly => None,
            Mode::Legacy(script) => Some(script),
            Mode::ConfigMode(config) => Some(&config.handler),
        }
    }
}

fn run_forever(mode: Mode) -> ! {
    let ctx = rusb::Context::new().expect("failed to create USB context");
    run(&mode, &mut UsbHost { ctx });
    unreachable!("the USB host never stops the event loop");
}

/// Release USB, run the queued handlers in order, and reclaim the device.
/// Returns None if the device cannot be reclaimed.
fn dispatch<H: Host>(
    host: &mut H,
    handle: H::Handle,
    script: &str,
    queue: &mut Dispatches,
) -> Option<H::Handle> {
    host.release(handle);
    for args in queue.drain(..) {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        host.run_handler(script, &args);
    }
    host.open()
}

fn run<H: Host>(mode: &Mode, host: &mut H) {
    let mut was_present = false;
    // Last accepted sample. Updated before any handler for it runs, so a
    // reconnect never replays transitions that were already dispatched.
    let mut prev: Option<State> = None;
    let mut gesture = GestureState::Idle;
    // Set after an interval in which the scanner was not observed (handler
    // handoff, reset). The next valid sample reconciles rather than diffs:
    // paper changes are absorbed, the button is compared with `prev`, and no
    // gesture timeout fires until that sample has been taken.
    let mut gap = false;
    let mut queue: Dispatches = Vec::new();

    while host.keep_running() {
        // ── Phase 1: wait for device ─────────────────────────────
        let mut handle = loop {
            if !host.keep_running() {
                return;
            }
            match host.open_with_reset() {
                Some(h) => break h,
                None => {
                    if was_present {
                        info!("{}", Event::DeviceLeft.tag());
                        if let Some(script) = mode.handler() {
                            host.run_handler(script, &[Event::DeviceLeft.tag()]);
                        }
                        was_present = false;
                        prev = None;
                        gesture = GestureState::Idle;
                        gap = false;
                    }
                    host.sleep(RECONNECT_INTERVAL);
                }
            }
        };

        if !was_present {
            info!("{}", Event::DeviceArrived.tag());
            was_present = true;
            if let Some(script) = mode.handler() {
                let mut arrived = vec![vec![Event::DeviceArrived.tag().to_string()]];
                match dispatch(host, handle, script, &mut arrived) {
                    Some(h) => handle = h,
                    None => continue,
                }
            }
        }

        // ── Phase 2: poll status while device is alive ───────────
        let mut poll_failures: u32 = 0;
        let mut has_reset = false;
        'poll: loop {
            if !host.keep_running() {
                return;
            }

            // A gesture only completes against a confirmed-present device.
            if !gap && poll_failures == 0 {
                check_gesture_timeout(&mut gesture, mode, host.now(), &mut queue);
            }
            if let (Some(script), false) = (mode.handler(), queue.is_empty()) {
                gap = true;
                match dispatch(host, handle, script, &mut queue) {
                    Some(h) => handle = h,
                    None => break 'poll,
                }
            }

            let state = match poll_status(&handle) {
                Ok(state) => state,
                Err(e) => {
                    poll_failures += 1;
                    if poll_failures < MAX_POLL_FAILURES {
                        debug!("poll failed ({poll_failures}/{MAX_POLL_FAILURES}): {e}, retrying");
                        host.sleep(POLL_INTERVAL);
                        continue 'poll;
                    }
                    debug!("poll failed ({poll_failures}/{MAX_POLL_FAILURES}): {e}");
                    // Whatever happens next, the failed interval was unobserved.
                    gap = true;
                    let recovered = if has_reset {
                        None
                    } else {
                        has_reset = true;
                        try_reset_device(host, handle)
                    };
                    let Some((new_handle, verified)) = recovered else {
                        debug!("poll failed, assuming device left");
                        break;
                    };
                    handle = new_handle;
                    // The verification sample reconciles like any post-gap sample.
                    verified
                }
            };
            poll_failures = 0;

            match prev {
                None => info!("initial: paper={} button={}", state.paper, state.button),
                Some(p) => {
                    let now = host.now();
                    process_transitions(p, state, mode, &mut gesture, now, gap, &mut queue);
                }
            }
            prev = Some(state);
            gap = false;

            if !queue.is_empty() {
                continue 'poll;
            }

            // In config mode with a pending gesture, poll faster to hit timeout promptly
            let sleep = match (mode, &gesture) {
                (Mode::ConfigMode(_), GestureState::Released(_, _)) => Duration::from_millis(20),
                _ => POLL_INTERVAL,
            };
            host.sleep(sleep);
        }
    }
}

/// Queue the scan for a completed gesture, if its press count is mapped.
fn finish_gesture(count: u32, config: &Config, queue: &mut Dispatches) {
    if let Some(profile) = config.profiles.get(&count) {
        info!("scan {} ({}x press)", profile, count);
        queue.push(vec!["scan".into(), profile.clone()]);
    } else {
        info!("{}x press — no profile mapped, ignoring", count);
    }
}

fn expired(ts: Instant, now: Instant, config: &Config) -> bool {
    now.duration_since(ts) >= config.gesture_timeout()
}

/// Finish the gesture if its window has expired.
fn check_gesture_timeout(
    gesture: &mut GestureState,
    mode: &Mode,
    now: Instant,
    queue: &mut Dispatches,
) {
    let Mode::ConfigMode(config) = mode else {
        return;
    };
    if let GestureState::Released(count, ts) = *gesture {
        if expired(ts, now, config) {
            *gesture = GestureState::Idle;
            finish_gesture(count, config, queue);
        }
    }
}

/// Apply every transition between two accepted samples, in order: paper, then
/// button. Handler invocations are queued, not run. After an observation gap
/// (`gap`), paper changes are absorbed because a handler may have caused them.
fn process_transitions(
    prev: State,
    curr: State,
    mode: &Mode,
    gesture: &mut GestureState,
    now: Instant,
    gap: bool,
    queue: &mut Dispatches,
) {
    for ev in transitions(prev, curr) {
        let is_paper = matches!(ev, Event::PaperIn | Event::PaperOut);
        if gap && is_paper {
            debug!("{} while unobserved, absorbed", ev.tag());
            continue;
        }
        match mode {
            Mode::ConfigMode(config) if !is_paper => {
                debug!("{}", ev.tag());
                *gesture = next_gesture(*gesture, ev, config, now, queue);
            }
            Mode::LogOnly => info!("{}", ev.tag()),
            _ => {
                info!("{}", ev.tag());
                queue.push(vec![ev.tag().into()]);
            }
        }
    }
}

fn next_gesture(
    gesture: GestureState,
    ev: Event,
    config: &Config,
    now: Instant,
    queue: &mut Dispatches,
) -> GestureState {
    match (ev, gesture) {
        (Event::ButtonDown, GestureState::Released(n, ts)) if expired(ts, now, config) => {
            finish_gesture(n, config, queue);
            debug!("gesture: press 1");
            GestureState::Pressed(1)
        }
        (Event::ButtonDown, GestureState::Released(n, _)) => {
            debug!("gesture: press {}", n + 1);
            GestureState::Pressed(n + 1)
        }
        (Event::ButtonDown, GestureState::Idle) => {
            debug!("gesture: press 1");
            GestureState::Pressed(1)
        }
        (Event::ButtonUp, GestureState::Pressed(n)) => {
            debug!("gesture: release {n}, waiting...");
            GestureState::Released(n, now)
        }
        // Down while pressed or up while not pressed: nothing to count.
        (_, other) => other,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Handle --help/--version/--doctor before logger init (they don't need it).
    match args.get(1).map(String::as_str) {
        Some("--help" | "-h") => {
            print_usage();
            std::process::exit(0);
        }
        Some("--version" | "-V") => {
            println!("s1500d {}", env!("CARGO_PKG_VERSION"));
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

    // RUST_LOG from environment wins; otherwise use config or default to "info".
    let log_filter = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| config.as_ref().map_or("info", |c| &c.log_level).to_string());

    env_logger::Builder::new()
        .parse_filters(&log_filter)
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
            run_forever(Mode::ConfigMode(config));
        }
        Some(h) => {
            info!("s1500d starting — handler: {h} (legacy mode)");
            run_forever(Mode::Legacy(h.to_string()));
        }
        None => {
            info!("s1500d starting — no handler (log only)");
            run_forever(Mode::LogOnly);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::test_config;

    // ── State::from_response ─────────────────────────────────────

    #[test]
    fn state_idle_scanner() {
        // byte 3 = 0x80 (hopper empty), byte 4 = 0x00 (button not pressed)
        let buf = [0, 0, 0, 0x80, 0x00, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf).unwrap();
        assert!(!s.paper);
        assert!(!s.button);
    }

    #[test]
    fn state_paper_present() {
        // byte 3 = 0x00 (bit 7 clear = paper present)
        let buf = [0, 0, 0, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf).unwrap();
        assert!(s.paper);
        assert!(!s.button);
    }

    #[test]
    fn state_button_held() {
        // byte 4 = 0x20 (bit 5 = button held)
        let buf = [0, 0, 0, 0x80, 0x20, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf).unwrap();
        assert!(!s.paper);
        assert!(s.button);
    }

    #[test]
    fn state_button_momentary_tap() {
        // byte 4 = 0x01 (bit 0 = momentary tap)
        let buf = [0, 0, 0, 0x80, 0x01, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf).unwrap();
        assert!(s.button);
    }

    #[test]
    fn state_button_both_bits() {
        // byte 4 = 0x21 (both button bits set)
        let buf = [0, 0, 0, 0x80, 0x21, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf).unwrap();
        assert!(s.button);
    }

    #[test]
    fn state_paper_and_button() {
        // byte 3 = 0x00 (paper present), byte 4 = 0x20 (button held)
        let buf = [0, 0, 0, 0x00, 0x20, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf).unwrap();
        assert!(s.paper);
        assert!(s.button);
    }

    #[test]
    fn state_short_buffer() {
        assert!(State::from_response(&[0, 0]).is_none());
        assert!(State::from_response(&[0, 0, 0, 0x80, 0x00]).is_none());
    }

    #[test]
    fn state_empty_buffer() {
        assert!(State::from_response(&[]).is_none());
    }

    #[test]
    fn state_other_bits_ignored() {
        // byte 3 has non-0x80 bits set but bit 7 is set → no paper
        let buf = [0, 0, 0, 0xFF, 0x00, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf).unwrap();
        assert!(!s.paper);

        // byte 4 has bits set but not 0x20 or 0x01 → no button
        let buf = [0, 0, 0, 0x80, 0xDE, 0, 0, 0, 0, 0, 0, 0];
        let s = State::from_response(&buf).unwrap();
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

    const NONE: State = State {
        paper: false,
        button: false,
    };
    const PAPER: State = State {
        paper: true,
        button: false,
    };
    const BUTTON: State = State {
        paper: false,
        button: true,
    };
    const BOTH: State = State {
        paper: true,
        button: true,
    };

    /// Run process_transitions and return the queued handler arguments.
    fn process(
        prev: State,
        curr: State,
        mode: &Mode,
        gesture: &mut GestureState,
        gap: bool,
    ) -> Dispatches {
        let mut queue = Vec::new();
        process_transitions(prev, curr, mode, gesture, Instant::now(), gap, &mut queue);
        queue
    }

    #[test]
    fn process_log_only_queues_nothing() {
        let mut gesture = GestureState::Idle;
        assert!(process(NONE, BOTH, &Mode::LogOnly, &mut gesture, false).is_empty());
    }

    #[test]
    fn process_legacy_queues_every_event_in_order() {
        let mut gesture = GestureState::Idle;
        let mode = Mode::Legacy("/bin/handler.sh".into());
        assert_eq!(
            process(NONE, BOTH, &mode, &mut gesture, false),
            [["paper-in"], ["button-down"]]
        );
        assert_eq!(
            process(BOTH, NONE, &mode, &mut gesture, false),
            [["paper-out"], ["button-up"]]
        );
    }

    #[test]
    fn process_config_button_down_starts_gesture() {
        let mut gesture = GestureState::Idle;
        let mode = Mode::ConfigMode(test_config());
        assert!(process(NONE, BUTTON, &mode, &mut gesture, false).is_empty());
        assert!(matches!(gesture, GestureState::Pressed(1)));
    }

    #[test]
    fn process_config_button_up_releases_gesture() {
        let mut gesture = GestureState::Pressed(1);
        let mode = Mode::ConfigMode(test_config());
        assert!(process(BUTTON, NONE, &mode, &mut gesture, false).is_empty());
        assert!(matches!(gesture, GestureState::Released(1, _)));
    }

    #[test]
    fn process_config_double_press() {
        let mut gesture = GestureState::Released(1, Instant::now());
        let mode = Mode::ConfigMode(test_config());
        assert!(process(NONE, BUTTON, &mode, &mut gesture, false).is_empty());
        assert!(matches!(gesture, GestureState::Pressed(2)));
    }

    #[test]
    fn process_config_press_after_expired_window_finishes_old_gesture() {
        let mut gesture = GestureState::Released(2, Instant::now() - Duration::from_secs(1));
        let mode = Mode::ConfigMode(test_config());
        assert_eq!(
            process(NONE, BUTTON, &mode, &mut gesture, false),
            [["scan", "legal"]]
        );
        assert!(matches!(gesture, GestureState::Pressed(1)));
    }

    #[test]
    fn process_config_paper_and_button_in_one_sample() {
        let mut gesture = GestureState::Idle;
        let mode = Mode::ConfigMode(test_config());
        assert_eq!(
            process(NONE, BOTH, &mode, &mut gesture, false),
            [["paper-in"]]
        );
        assert!(matches!(gesture, GestureState::Pressed(1)));
    }

    #[test]
    fn process_gap_absorbs_paper_but_not_button() {
        let mut gesture = GestureState::Idle;
        let mode = Mode::Legacy("/bin/handler.sh".into());
        assert_eq!(
            process(BUTTON, PAPER, &mode, &mut gesture, true),
            [["button-up"]]
        );
    }

    #[test]
    fn process_no_change_queues_nothing() {
        let mut gesture = GestureState::Idle;
        let mode = Mode::Legacy("/bin/handler.sh".into());
        assert!(process(NONE, NONE, &mode, &mut gesture, false).is_empty());
    }

    // ── check_gesture_timeout ────────────────────────────────────

    fn timeout(gesture: GestureState, mode: &Mode) -> (GestureState, Dispatches) {
        let mut gesture = gesture;
        let mut queue = Vec::new();
        check_gesture_timeout(&mut gesture, mode, Instant::now(), &mut queue);
        (gesture, queue)
    }

    fn expired_release(count: u32) -> GestureState {
        GestureState::Released(count, Instant::now() - Duration::from_secs(1))
    }

    #[test]
    fn gesture_timeout_not_config_mode() {
        let (gesture, queue) = timeout(expired_release(1), &Mode::LogOnly);
        assert!(queue.is_empty());
        assert!(matches!(gesture, GestureState::Released(1, _)));
    }

    #[test]
    fn gesture_timeout_not_released() {
        let mode = Mode::ConfigMode(test_config());
        let (gesture, queue) = timeout(GestureState::Pressed(1), &mode);
        assert!(queue.is_empty());
        assert!(matches!(gesture, GestureState::Pressed(1)));
    }

    #[test]
    fn gesture_timeout_not_expired() {
        let mode = Mode::ConfigMode(test_config());
        let (gesture, queue) = timeout(GestureState::Released(1, Instant::now()), &mode);
        assert!(queue.is_empty());
        assert!(matches!(gesture, GestureState::Released(1, _)));
    }

    #[test]
    fn gesture_timeout_expired_mapped() {
        let mode = Mode::ConfigMode(test_config());
        let (gesture, queue) = timeout(expired_release(1), &mode);
        assert_eq!(queue, [["scan", "standard"]]);
        assert!(matches!(gesture, GestureState::Idle));
    }

    #[test]
    fn gesture_timeout_expired_double_press() {
        let mode = Mode::ConfigMode(test_config());
        let (_, queue) = timeout(expired_release(2), &mode);
        assert_eq!(queue, [["scan", "legal"]]);
    }

    #[test]
    fn gesture_timeout_expired_unmapped() {
        let mode = Mode::ConfigMode(test_config());
        let (gesture, queue) = timeout(expired_release(5), &mode);
        assert!(queue.is_empty());
        assert!(matches!(gesture, GestureState::Idle));
    }
}
