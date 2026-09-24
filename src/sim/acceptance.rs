//! Acceptance traces for s1500d-h66 and s1500d-0xx.2, written against the
//! agreed dispatch contract before the implementation.

use super::*;

const BOTH: State = State {
    paper: true,
    button: true,
};

// Ignore lifecycle here so an unrelated arrival-ownership failure cannot hide
// whether a sensor/gesture regression is independently fixed.
fn sensor_handlers(sim: &Sim) -> Vec<String> {
    sim.handlers()
        .into_iter()
        .filter(|s| !s.starts_with("device-"))
        .collect()
}

fn config_mode() -> Mode {
    Mode::ConfigMode(test_config())
}

#[test]
fn arrival_releases_before_handler_and_reclaims() {
    let sim = simulate(&legacy(), vec![Step::Poll(IDLE), Step::Poll(IDLE)]);
    assert_eq!(
        sim.trace(),
        [
            "open+reset",
            "release",
            "handler device-arrived (released)",
            "open"
        ]
    );
}

#[test]
fn arrival_disappearance_emits_left_without_claim() {
    use Step::*;
    let sim = simulate(
        &legacy(),
        vec![Handler(Duration::from_millis(100)), Present(false)],
    );
    assert_eq!(
        sim.handlers(),
        ["device-arrived (released)", "device-left (released)"]
    );
}

#[test]
fn arrival_reclaim_failure_immediate_reopen_does_not_repeat_arrival() {
    use Step::*;
    let sim = simulate(
        &legacy(),
        vec![Handler(Duration::ZERO), OpenFails, Poll(IDLE)],
    );
    assert_eq!(sim.handlers(), ["device-arrived (released)"]);
    assert_eq!(sim.trace().iter().filter(|s| *s == "open+reset").count(), 2);
}

#[test]
fn legacy_simultaneous_edges_keep_paper_then_button_order() {
    use Step::*;
    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            Poll(IDLE),
            Poll(BOTH),
            Poll(BOTH),
            Poll(IDLE),
            Poll(IDLE),
        ],
    );
    assert_eq!(
        sensor_handlers(&sim),
        [
            "paper-in (released)",
            "button-down (released)",
            "paper-out (released)",
            "button-up (released)"
        ]
    );
}

#[test]
fn config_simultaneous_edges_complete_gesture() {
    use Step::*;
    let sim = simulate(
        &config_mode(),
        vec![
            Poll(IDLE),
            Poll(IDLE),
            Poll(BOTH),
            Poll(BOTH),
            Poll(IDLE),
            Poll(IDLE),
        ],
    );
    assert_eq!(
        sensor_handlers(&sim),
        [
            "paper-in (released)",
            "paper-out (released)",
            "scan standard (released)"
        ]
    );
}

#[test]
fn legacy_release_inside_down_handler_delivers_up() {
    use Step::*;
    let sim = simulate(
        &legacy(),
        vec![
            Poll(IDLE),
            Poll(IDLE),
            Poll(HELD),
            Handler(Duration::from_millis(300)),
            Set(IDLE),
        ],
    );
    assert_eq!(
        sensor_handlers(&sim),
        ["button-down (released)", "button-up (released)"]
    );
}

#[test]
fn config_release_inside_paper_handler_completes_gesture() {
    use Step::*;
    let sim = simulate(
        &config_mode(),
        vec![
            Poll(IDLE),
            Poll(IDLE),
            Poll(HELD),
            Poll(BOTH),
            Handler(Duration::from_millis(300)),
            Set(PAPER),
        ],
    );
    assert_eq!(
        sensor_handlers(&sim),
        ["paper-in (released)", "scan standard (released)"]
    );
}

#[test]
fn paper_consumed_by_paper_handler_is_absorbed_in_both_modes() {
    use Step::*;
    for mode in [legacy(), config_mode()] {
        for failed_reclaim in [false, true] {
            let mut steps = vec![
                Poll(IDLE),
                Poll(IDLE),
                Poll(PAPER),
                Handler(Duration::from_millis(200)),
                Set(IDLE),
            ];
            if failed_reclaim {
                steps.push(OpenFails);
            }
            steps.push(Poll(IDLE));
            let sim = simulate(&mode, steps);
            assert_eq!(
                sensor_handlers(&sim),
                ["paper-in (released)"],
                "failed_reclaim={failed_reclaim}"
            );
        }
    }
}

#[test]
fn paper_consumed_by_legacy_button_handler_is_absorbed() {
    use Step::*;
    for failed_reclaim in [false, true] {
        let mut steps = vec![
            Poll(PAPER),
            Poll(PAPER),
            Poll(BOTH),
            Handler(Duration::from_millis(200)),
            Set(IDLE),
        ];
        if failed_reclaim {
            steps.push(OpenFails);
        }
        steps.push(Poll(IDLE));
        let sim = simulate(&legacy(), steps);
        assert_eq!(
            sensor_handlers(&sim),
            ["button-down (released)", "button-up (released)"],
            "failed_reclaim={failed_reclaim}"
        );
    }
}

#[test]
fn accepted_paper_in_never_replays_after_either_handoff_failure() {
    use Step::*;
    for failed_baseline in [false, true] {
        let failure = if failed_baseline { FailPoll } else { OpenFails };
        let sim = simulate(
            &legacy(),
            vec![Poll(IDLE), Poll(IDLE), Poll(PAPER), failure, Poll(PAPER)],
        );
        assert_eq!(
            sensor_handlers(&sim),
            ["paper-in (released)"],
            "failed_baseline={failed_baseline}"
        );
    }
}

fn expired_gap_prefix(failed_baseline: bool) -> Vec<Step> {
    use Step::*;
    vec![
        Poll(IDLE),
        Poll(IDLE),
        Poll(HELD),
        Poll(IDLE),
        Poll(PAPER),
        Handler(Duration::from_secs(1)),
        if failed_baseline { FailPoll } else { OpenFails },
    ]
}

#[test]
fn expired_gesture_reopen_held_preserves_new_press_through_old_scan() {
    use Step::*;
    for failed_baseline in [false, true] {
        let mut steps = expired_gap_prefix(failed_baseline);
        steps.extend([Poll(HELD), Poll(HELD), Poll(IDLE)]);
        let sim = simulate(&config_mode(), steps);
        assert_eq!(
            sensor_handlers(&sim),
            [
                "paper-in (released)",
                "scan standard (released)",
                "scan standard (released)"
            ],
            "failed_baseline={failed_baseline}"
        );
    }
}

#[test]
fn expired_gesture_reopen_up_finalizes_once_after_valid_sample() {
    use Step::*;
    for failed_baseline in [false, true] {
        let mut steps = expired_gap_prefix(failed_baseline);
        steps.extend([Poll(IDLE), Poll(IDLE)]);
        let sim = simulate(&config_mode(), steps);
        assert_eq!(
            sensor_handlers(&sim),
            ["paper-in (released)", "scan standard (released)"],
            "failed_baseline={failed_baseline}"
        );
    }
}

#[test]
fn no_expired_scan_when_reopened_device_never_produces_valid_baseline() {
    use Step::*;
    let mut steps = expired_gap_prefix(false);
    steps.extend([FailPoll, FailPoll, FailPoll, Present(false)]);
    let sim = simulate(&config_mode(), steps);
    assert_eq!(sensor_handlers(&sim), ["paper-in (released)"]);
    assert!(sim
        .handlers()
        .contains(&"device-left (released)".to_string()));
}

#[test]
fn unexpired_reopen_down_continues_double_press() {
    use Step::*;
    let sim = simulate(
        &config_mode(),
        vec![
            Poll(IDLE),
            Poll(IDLE),
            Poll(HELD),
            Poll(IDLE),
            Poll(PAPER),
            Handler(Duration::from_millis(100)),
            OpenFails,
            Poll(HELD),
            Poll(IDLE),
        ],
    );
    assert_eq!(
        sensor_handlers(&sim),
        ["paper-in (released)", "scan legal (released)"]
    );
}

#[test]
fn completed_scan_absorbs_paper_removal_and_does_not_rescan() {
    use Step::*;
    let sim = simulate(
        &config_mode(),
        vec![
            Poll(PAPER),
            Poll(PAPER),
            Poll(BOTH),
            Poll(PAPER),
            // Wait starts after the first 20 ms gesture sleep. End exactly
            // at the 600 ms deadline so the next expected action is dispatch.
            Wait(Duration::from_millis(580)),
            Handler(Duration::from_millis(300)),
            Set(IDLE),
        ],
    );
    assert_eq!(sensor_handlers(&sim), ["scan standard (released)"]);
}

#[test]
fn unobserved_complete_button_cycle_does_not_fabricate_scan() {
    use Step::*;
    let sim = simulate(
        &config_mode(),
        vec![
            Poll(IDLE),
            Poll(IDLE),
            Poll(PAPER),
            Handler(Duration::from_millis(300)),
            Set(BOTH),
            Set(PAPER),
        ],
    );
    assert_eq!(sensor_handlers(&sim), ["paper-in (released)"]);
}
