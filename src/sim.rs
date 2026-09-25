//! Scripted host for event-loop tests.
//!
//! Drives the production event loop against a simulated scanner. The
//! script is consumed in order: transaction steps answer polls, device
//! steps change the hardware between transactions. When no step applies,
//! a present scanner keeps reporting its last state. The run ends five
//! simulated seconds after the script is exhausted.

use std::collections::HashMap;

use super::*;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

mod acceptance;

pub(crate) fn test_config() -> Config {
    Config {
        handler: "/bin/test-handler.sh".into(),
        gesture_timeout_ms: 600,
        log_level: "info".into(),
        profiles: HashMap::from([(1, "standard".into()), (2, "legal".into())]),
    }
}

const IDLE: State = State {
    paper: false,
    button: false,
};
const PAPER: State = State {
    paper: true,
    button: false,
};
const HELD: State = State {
    paper: false,
    button: true,
};

enum Io {
    Write(rusb::Result<usize>),
    Read(rusb::Result<Vec<u8>>),
}

enum Step {
    /// One complete, valid GET_HW_STATUS transaction.
    Poll(State),
    /// Let simulated time pass; the scanner keeps its current state.
    Wait(Duration),
    /// One transaction whose command write times out.
    FailPoll,
    /// A single raw USB operation.
    Io(Io),
    /// Power the scanner on (true) or off (false).
    Present(bool),
    /// The next open fails although the device is present.
    OpenFails,
    /// Change sensor state without a poll (e.g. while a handler runs).
    Set(State),
    /// Consumed by the next handler run: it takes this long. Device steps
    /// that follow it happen while the handler runs.
    Handler(Duration),
}

fn frame(s: State) -> Vec<u8> {
    let hopper = if s.paper { 0x00 } else { 0x80 };
    let button = if s.button { 0x20 } else { 0x00 };
    vec![0, 0, 0, hopper, button, 0x01, 0x80, 0, 0, 0, 0, 0]
}

/// Literal fixtures, independent of the production constants.
const GHS_COMMAND: [u8; 31] = [
    0x43, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // envelope
    0xC2, 0, 0, 0, 0, 0, 0, 0, 0x0C, 0, // GET_HW_STATUS, 12 bytes
    0, 0,
];

fn status(code: u8) -> Vec<u8> {
    vec![0x53, 0, 0, 0, 0, 0, 0, 0, 0, code, 0, 0, 0]
}

fn transaction(s: State) -> Vec<Io> {
    vec![
        Io::Write(Ok(31)),
        Io::Read(Ok(frame(s))),
        Io::Read(Ok(status(0))),
    ]
}

struct Device {
    steps: VecDeque<Step>,
    pending: VecDeque<Io>,
    wait_until: Option<Instant>,
    drained_at: Option<Instant>,
    stopped: bool,
    last: State,
    present: bool,
    claims: u32,
    now: Instant,
    trace: Vec<String>,
    calls: u32,
    reads_since_write: u32,
}

impl Device {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: steps.into(),
            pending: VecDeque::new(),
            wait_until: None,
            drained_at: None,
            stopped: false,
            last: IDLE,
            present: true,
            claims: 0,
            now: Instant::now(),
            trace: Vec::new(),
            calls: 0,
            reads_since_write: 0,
        }
    }

    /// Apply hardware changes and elapsed waits due before the next
    /// call. Never interrupts a transaction in progress.
    fn settle(&mut self) {
        self.calls += 1;
        assert!(self.calls < 100_000, "script never finished");
        if !self.pending.is_empty() {
            return;
        }
        loop {
            match self.steps.front() {
                Some(Step::Present(p)) => self.present = *p,
                Some(Step::Set(s)) => self.last = *s,
                Some(Step::Wait(d)) => {
                    let until = *self.wait_until.get_or_insert(self.now + *d);
                    if self.now < until {
                        break;
                    }
                    self.wait_until = None;
                }
                _ => break,
            }
            self.steps.pop_front();
        }
    }

    fn next_io(&mut self) -> Io {
        self.settle();
        if !self.present {
            return Io::Write(Err(rusb::Error::NoDevice));
        }
        if self.pending.is_empty() {
            match self.steps.front() {
                Some(Step::Poll(s)) => {
                    self.last = *s;
                    self.pending = transaction(*s).into();
                    self.steps.pop_front();
                }
                Some(Step::FailPoll) => {
                    self.pending.push_back(Io::Write(Err(rusb::Error::Timeout)));
                    self.steps.pop_front();
                }
                Some(Step::Io(_)) => {
                    let Some(Step::Io(io)) = self.steps.pop_front() else {
                        unreachable!()
                    };
                    self.pending.push_back(io);
                }
                Some(Step::OpenFails) => panic!("script expected an open, got USB I/O"),
                Some(Step::Handler(_)) => panic!("script expected a handler, got USB I/O"),
                Some(Step::Present(_) | Step::Set(_)) => unreachable!(),
                Some(Step::Wait(_)) | None => self.pending = transaction(self.last).into(),
            }
        }
        self.pending.pop_front().unwrap()
    }

    fn claim(&mut self, label: &str) -> bool {
        self.settle();
        if let Some(Step::OpenFails) = self.steps.front() {
            self.steps.pop_front();
            self.trace.push(format!("{label} failed"));
            return false;
        }
        if !self.present {
            return false;
        }
        self.claims += 1;
        self.trace.push(label.into());
        true
    }
}

struct FakeHandle {
    dev: Rc<RefCell<Device>>,
    released: Cell<bool>,
}

impl Drop for FakeHandle {
    fn drop(&mut self) {
        let mut dev = self.dev.borrow_mut();
        dev.claims -= 1;
        if !self.released.get() && !dev.stopped {
            dev.trace.push("close".into());
        }
    }
}

impl Link for FakeHandle {
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> rusb::Result<usize> {
        assert_eq!(endpoint, 0x02);
        assert_eq!(buf, GHS_COMMAND);
        assert_eq!(timeout, Duration::from_millis(1000));
        let mut dev = self.dev.borrow_mut();
        dev.reads_since_write = 0;
        match dev.next_io() {
            Io::Write(r) => r,
            Io::Read(_) => panic!("script expected a read, got a write"),
        }
    }

    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> rusb::Result<usize> {
        assert_eq!(endpoint, 0x81);
        let mut dev = self.dev.borrow_mut();
        // Data phase waits 1 s; the status phase 200 ms.
        let want = if dev.reads_since_write == 0 {
            1000
        } else {
            200
        };
        assert_eq!(timeout, Duration::from_millis(want));
        dev.reads_since_write += 1;
        match dev.next_io() {
            Io::Read(r) => r.map(|data| {
                buf[..data.len()].copy_from_slice(&data);
                data.len()
            }),
            // A write-shaped failure (device gone, script exhausted).
            Io::Write(Err(e)) => Err(e),
            Io::Write(Ok(_)) => panic!("script expected a write, got a read"),
        }
    }
}

struct Sim {
    dev: Rc<RefCell<Device>>,
}

impl Sim {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            dev: Rc::new(RefCell::new(Device::new(steps))),
        }
    }

    fn handle(&self, label: &str) -> Option<FakeHandle> {
        self.dev.borrow_mut().claim(label).then(|| FakeHandle {
            dev: Rc::clone(&self.dev),
            released: Cell::new(false),
        })
    }

    fn trace(&self) -> Vec<String> {
        self.dev.borrow().trace.clone()
    }

    /// Just the handler invocations, e.g. "paper-in (released)".
    fn handlers(&self) -> Vec<String> {
        self.trace()
            .into_iter()
            .filter_map(|t| t.strip_prefix("handler ").map(String::from))
            .collect()
    }
}

impl Host for Sim {
    type Handle = FakeHandle;

    fn open(&mut self) -> Option<FakeHandle> {
        self.handle("open")
    }

    fn open_with_reset(&mut self) -> Option<FakeHandle> {
        self.handle("open+reset")
    }

    fn reset(&mut self, handle: FakeHandle) -> Option<FakeHandle> {
        drop(handle);
        self.dev.borrow_mut().trace.push("reset".into());
        self.handle("open")
    }

    fn release(&mut self, handle: FakeHandle) {
        handle.released.set(true);
        self.dev.borrow_mut().trace.push("release".into());
    }

    fn run_handler(&mut self, _script: &str, args: &[&str]) {
        let mut dev = self.dev.borrow_mut();
        if let Some(Step::Handler(d)) = dev.steps.front() {
            let d = *d;
            dev.steps.pop_front();
            dev.now += d;
            dev.settle();
        }
        let usb = if dev.claims == 0 {
            "released"
        } else {
            "claimed"
        };
        dev.trace
            .push(format!("handler {} ({usb})", args.join(" ")));
    }

    fn sleep(&mut self, duration: Duration) {
        self.dev.borrow_mut().now += duration;
    }

    fn now(&self) -> Instant {
        self.dev.borrow().now
    }

    fn keep_running(&mut self) -> bool {
        let mut dev = self.dev.borrow_mut();
        dev.settle();
        if !(dev.steps.is_empty() && dev.pending.is_empty()) {
            return true;
        }
        let now = dev.now;
        let drained = *dev.drained_at.get_or_insert(now);
        dev.stopped = now.duration_since(drained) >= Duration::from_secs(5);
        !dev.stopped
    }
}

fn legacy() -> Mode {
    Mode::Legacy("/bin/handler.sh".into())
}

fn simulate(mode: &Mode, steps: Vec<Step>) -> Sim {
    let mut sim = Sim::new(steps);
    run(mode, &mut sim);
    sim
}

// ── Event loop: lifecycle and recovery ───────────────────────

#[test]
fn loop_connect_then_power_off() {
    use Step::*;
    let sim = simulate(&legacy(), vec![Poll(IDLE), Poll(IDLE), Present(false)]);
    assert_eq!(
        sim.trace(),
        [
            "open+reset",
            "release",
            "handler device-arrived (released)",
            "open",
            // three failed polls, then a reset that cannot reopen
            "close",
            "reset",
            "handler device-left (released)",
        ]
    );
}

#[test]
fn loop_power_cycle_reconnects() {
    use Step::*;
    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            Present(false),
            Present(true),
            Poll(IDLE),
            Poll(PAPER),
        ],
    );
    // Present(false) and Present(true) settle together, so the device is
    // back before the first failed poll is observed; no left/arrived.
    assert_eq!(
        sim.handlers(),
        ["device-arrived (released)", "paper-in (released)"]
    );

    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            FailPoll,
            FailPoll,
            FailPoll,
            Present(false),
            Present(true),
            Poll(IDLE), // verification poll after reset
            Poll(PAPER),
        ],
    );
    assert_eq!(
        sim.handlers(),
        ["device-arrived (released)", "paper-in (released)"]
    );
}

#[test]
fn loop_left_and_arrived_across_real_absence() {
    use Step::*;
    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            Present(false),
            // Absent across at least one reconnect attempt.
            Wait(Duration::from_secs(3)),
            Present(true),
            Poll(IDLE),
            Poll(PAPER),
        ],
    );
    assert_eq!(
        sim.handlers(),
        [
            "device-arrived (released)",
            "device-left (released)",
            "device-arrived (released)",
            "paper-in (released)",
        ]
    );
}

#[test]
fn loop_transient_poll_failures_do_not_reset() {
    use Step::*;
    let sim = simulate(&legacy(), vec![Poll(IDLE), FailPoll, FailPoll, Poll(PAPER)]);
    assert!(!sim.trace().contains(&"reset".to_string()));
    assert_eq!(
        sim.handlers(),
        ["device-arrived (released)", "paper-in (released)"]
    );
}

#[test]
fn loop_persistent_failure_reset_succeeds() {
    use Step::*;
    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            FailPoll,
            FailPoll,
            FailPoll,
            Poll(PAPER), // verification poll after reset: reconciles, absorbed
            Poll(IDLE),
        ],
    );
    assert_eq!(
        sim.trace(),
        [
            "open+reset",
            "release",
            "handler device-arrived (released)",
            "open",
            "close",
            "reset",
            "open",
            "release",
            "handler paper-out (released)",
            "open",
        ]
    );
}

#[test]
fn loop_reset_unresponsive_reopens_without_events() {
    use Step::*;
    // An unresponsive device after reset breaks to the reconnect loop; an
    // immediate reopen emits neither left nor arrived, and the first sample
    // after it reconciles (the paper change is absorbed).
    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            FailPoll,
            FailPoll,
            FailPoll,
            FailPoll, // verification poll after reset
            Poll(PAPER),
        ],
    );
    assert_eq!(sim.handlers(), ["device-arrived (released)"]);
    assert_eq!(sim.trace().iter().filter(|t| *t == "open+reset").count(), 2);
}

#[test]
fn loop_second_wedge_in_same_connection_skips_reset() {
    use Step::*;
    // has_reset is scoped to one Phase-2 entry: after a successful reset,
    // the next persistent failure goes straight to the reconnect loop.
    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            FailPoll,
            FailPoll,
            FailPoll,
            Poll(IDLE), // verification
            FailPoll,
            FailPoll,
            FailPoll,
            Poll(IDLE),
        ],
    );
    let trace = sim.trace();
    assert_eq!(trace.iter().filter(|t| *t == "reset").count(), 1);
    assert_eq!(trace.iter().filter(|t| *t == "open+reset").count(), 2);
}

// ── Event loop: handler handoff ──────────────────────────────

#[test]
fn loop_sensor_handler_runs_released_then_reclaims() {
    use Step::*;
    let sim = simulate(&legacy(), vec![Poll(IDLE), Poll(PAPER), Poll(PAPER)]);
    assert_eq!(
        sim.trace(),
        [
            "open+reset",
            "release",
            "handler device-arrived (released)",
            "open",
            "release",
            "handler paper-in (released)",
            "open",
        ]
    );
}

#[test]
fn loop_power_off_during_handler_emits_left() {
    use Step::*;
    let sim = simulate(&legacy(), vec![Poll(IDLE), Poll(PAPER), Present(false)]);
    assert_eq!(
        sim.handlers(),
        [
            "device-arrived (released)",
            "paper-in (released)",
            "device-left (released)",
        ]
    );
}

#[test]
fn loop_reclaim_failure_reopens_without_replay() {
    use Step::*;
    let sim = simulate(
        &legacy(),
        vec![Poll(IDLE), Poll(PAPER), OpenFails, Poll(PAPER)],
    );
    // The baseline advanced before the handler ran, so the immediate reopen
    // does not dispatch the same paper-in again.
    assert_eq!(
        sim.trace(),
        [
            "open+reset",
            "release",
            "handler device-arrived (released)",
            "open",
            "release",
            "handler paper-in (released)",
            "open failed",
            "open+reset",
        ]
    );
}

#[test]
fn loop_poll_failure_after_handler_is_transient() {
    use Step::*;
    // The first poll after a handoff is an ordinary poll: one failure is
    // retried, not treated as a lost device, and nothing replays.
    let sim = simulate(
        &legacy(),
        vec![Poll(IDLE), Poll(PAPER), FailPoll, Poll(PAPER)],
    );
    assert_eq!(
        sim.trace(),
        [
            "open+reset",
            "release",
            "handler device-arrived (released)",
            "open",
            "release",
            "handler paper-in (released)",
            "open",
        ]
    );
}

// ── Event loop: gestures ─────────────────────────────────────

#[test]
fn loop_single_press_dispatches_after_timeout() {
    use Step::*;
    let mode = Mode::ConfigMode(test_config());
    let sim = simulate(&mode, vec![Poll(IDLE), Poll(HELD), Poll(IDLE)]);
    assert_eq!(
        sim.handlers(),
        ["device-arrived (released)", "scan standard (released)"]
    );
}

#[test]
fn loop_double_press_dispatches_second_profile() {
    use Step::*;
    let mode = Mode::ConfigMode(test_config());
    let sim = simulate(
        &mode,
        vec![Poll(IDLE), Poll(HELD), Poll(IDLE), Poll(HELD), Poll(IDLE)],
    );
    assert_eq!(
        sim.handlers(),
        ["device-arrived (released)", "scan legal (released)"]
    );
}

#[test]
fn loop_gesture_waits_for_timeout() {
    use Step::*;
    let mode = Mode::ConfigMode(test_config());
    let sim = simulate(
        &mode,
        vec![
            Poll(IDLE),
            Poll(HELD),
            Poll(IDLE),
            Wait(Duration::from_millis(500)),
            Poll(HELD),
            Poll(IDLE),
        ],
    );
    // A second press 500 ms after release still belongs to the gesture.
    assert_eq!(
        sim.handlers(),
        ["device-arrived (released)", "scan legal (released)"]
    );
}

#[test]
fn loop_unmapped_press_count_runs_nothing() {
    use Step::*;
    let mode = Mode::ConfigMode(test_config());
    let mut steps = vec![Poll(IDLE)];
    for _ in 0..3 {
        steps.extend([Poll(HELD), Poll(IDLE)]);
    }
    let sim = simulate(&mode, steps);
    assert_eq!(sim.handlers(), ["device-arrived (released)"]);
}

#[test]
fn loop_gesture_expires_during_paper_handler() {
    use Step::*;
    // Released(1) is pending when paper-in dispatches; the handler takes
    // longer than the gesture window, so the scan fires before the next
    // poll and its baseline absorbs the paper removal. An instant handler
    // would dispatch paper-out first.
    let mode = Mode::ConfigMode(test_config());
    let sim = simulate(
        &mode,
        vec![
            Poll(IDLE),
            Poll(HELD),
            Poll(IDLE),
            Poll(PAPER),
            Handler(Duration::from_secs(1)),
            Poll(PAPER), // baseline after paper-in
            Poll(IDLE),  // paper removed
        ],
    );
    assert_eq!(
        sim.trace(),
        [
            "open+reset",
            "release",
            "handler device-arrived (released)",
            "open",
            "release",
            "handler paper-in (released)",
            "open",
            "release",
            "handler scan standard (released)",
            "open",
        ]
    );
}

#[test]
fn loop_power_off_while_handler_runs() {
    use Step::*;
    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            Poll(PAPER),
            Handler(Duration::from_secs(2)),
            Present(false),
        ],
    );
    assert_eq!(
        sim.handlers(),
        [
            "device-arrived (released)",
            "paper-in (released)",
            "device-left (released)",
        ]
    );
}

#[test]
fn loop_unplug_during_gesture_window_cancels_gesture() {
    use Step::*;
    // Unplugged 500 ms into the window: polls fail past the deadline, but
    // the gesture does not complete against an unconfirmed device, and the
    // next connection starts with no gesture.
    let mode = Mode::ConfigMode(test_config());
    let sim = simulate(
        &mode,
        vec![
            Poll(IDLE),
            Poll(HELD),
            Poll(IDLE),
            Wait(Duration::from_millis(500)),
            Present(false),
            Wait(Duration::from_secs(3)),
            Present(true),
            Poll(IDLE),
        ],
    );
    assert_eq!(
        sim.handlers(),
        [
            "device-arrived (released)",
            "device-left (released)",
            "device-arrived (released)",
        ]
    );
}

#[test]
fn loop_transient_failure_does_not_absorb_paper() {
    use Step::*;
    let sim = simulate(&legacy(), vec![Poll(IDLE), FailPoll, Poll(PAPER)]);
    assert_eq!(
        sim.handlers(),
        ["device-arrived (released)", "paper-in (released)"]
    );
}

#[test]
fn loop_log_only_never_releases_or_dispatches() {
    use Step::*;
    let sim = simulate(
        &Mode::LogOnly,
        vec![
            Poll(IDLE),
            Poll(State {
                paper: true,
                button: true,
            }),
            Poll(IDLE),
            FailPoll,
            FailPoll,
            FailPoll,
            Poll(IDLE), // verification after reset
            Poll(PAPER),
        ],
    );
    assert_eq!(sim.trace(), ["open+reset", "close", "reset", "open"]);
}

#[test]
fn loop_reset_verification_sample_is_observed() {
    use Step::*;
    // The release is first seen by the post-reset verification poll; it
    // must count, so the next press makes a double press.
    let mode = Mode::ConfigMode(test_config());
    let sim = simulate(
        &mode,
        vec![
            Poll(IDLE),
            Poll(IDLE),
            Poll(HELD),
            FailPoll,
            FailPoll,
            FailPoll,
            Poll(IDLE), // verification observes the release
            Poll(HELD),
            Poll(IDLE),
        ],
    );
    assert_eq!(
        sim.handlers(),
        ["device-arrived (released)", "scan legal (released)"]
    );

    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            Poll(HELD),
            FailPoll,
            FailPoll,
            FailPoll,
            Poll(IDLE), // verification observes the release
            Poll(HELD),
        ],
    );
    assert_eq!(
        sim.handlers(),
        [
            "device-arrived (released)",
            "button-down (released)",
            "button-up (released)",
            "button-down (released)",
        ]
    );
}

// ── poll_status transaction validation ───────────────────────

fn poll_raw(ios: Vec<Io>) -> Result<State, PollError> {
    let sim = Sim::new(ios.into_iter().map(Step::Io).collect());
    let handle = sim.handle("open").unwrap();
    poll_status(&handle)
}

#[test]
fn poll_valid_transaction() {
    assert_eq!(poll_raw(transaction(PAPER)), Ok(PAPER));
}

#[test]
fn poll_rejects_failed_write() {
    let r = poll_raw(vec![Io::Write(Err(rusb::Error::Pipe))]);
    assert_eq!(r, Err(PollError::Write(rusb::Error::Pipe)));
}

#[test]
fn poll_rejects_short_write() {
    assert_eq!(
        poll_raw(vec![Io::Write(Ok(19))]),
        Err(PollError::ShortWrite(19))
    );
}

#[test]
fn poll_rejects_short_data() {
    let r = poll_raw(vec![
        Io::Write(Ok(31)),
        Io::Read(Ok(frame(PAPER)[..5].to_vec())),
        Io::Read(Ok(status(0))),
    ]);
    assert_eq!(r, Err(PollError::BadDataLength(5)));
}

#[test]
fn poll_rejects_status_in_data_phase() {
    // Previously decoded as paper=true, button=false.
    let r = poll_raw(vec![Io::Write(Ok(31)), Io::Read(Ok(status(2)))]);
    assert_eq!(r, Err(PollError::StatusInData(status(2))));
}

#[test]
fn poll_rejects_missing_status_phase() {
    let r = poll_raw(vec![
        Io::Write(Ok(31)),
        Io::Read(Ok(frame(PAPER))),
        Io::Read(Err(rusb::Error::Timeout)),
    ]);
    assert_eq!(r, Err(PollError::ReadStatus(rusb::Error::Timeout)));
}

#[test]
fn poll_rejects_error_status() {
    // SCSI CHECK CONDITION (2) and BUSY (8) in the status envelope.
    for code in [2, 8] {
        let r = poll_raw(vec![
            Io::Write(Ok(31)),
            Io::Read(Ok(frame(PAPER))),
            Io::Read(Ok(status(code))),
        ]);
        assert_eq!(r, Err(PollError::BadStatus(status(code))));
    }
}

#[test]
fn poll_rejects_malformed_status() {
    let mut wrong_code = status(0);
    wrong_code[0] = 0x43;
    for bad in [status(0)[..12].to_vec(), wrong_code, vec![]] {
        let r = poll_raw(vec![
            Io::Write(Ok(31)),
            Io::Read(Ok(frame(PAPER))),
            Io::Read(Ok(bad.clone())),
        ]);
        assert_eq!(r, Err(PollError::BadStatus(bad)));
    }
}

#[test]
fn loop_status_in_data_does_not_fabricate_paper() {
    use Step::*;
    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            Io(self::Io::Write(Ok(31))),
            Io(self::Io::Read(Ok(status(2)))),
            Poll(IDLE),
        ],
    );
    assert_eq!(sim.handlers(), ["device-arrived (released)"]);
}
