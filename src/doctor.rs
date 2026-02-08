use std::io::{self, BufRead, Write as IoWrite};
use std::time::Duration;

use crate::{poll_status, try_open, State};

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
    let start = std::time::Instant::now();
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
        std::thread::sleep(crate::POLL_INTERVAL);
    }
}

pub fn doctor() {
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
