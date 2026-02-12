// Author: Dustin Pilgrim
// License: MIT

use crate::core::action::Action;
use crate::core::config::{Config, ConfigFile, PlanSource, PlanStep, PlanStepKind};
use crate::core::events::{ActivityKind, Event};
use crate::core::manager::Manager;
use crate::core::state::State;

fn cfg_with_plan(plan: Vec<PlanStep>) -> ConfigFile {
    let mut cfg = Config::disabled();

    // effective_for() selects plan_* into cfg.plan; tests must populate plan_desktop.
    cfg.plan_desktop = plan;

    ConfigFile {
        default: cfg,
        profiles: vec![],
        active_profile: None,
    }
}

fn cfg_with_plan_and_notify(
    plan: Vec<PlanStep>,
    debounce_seconds: u64,
    notify_before_action: bool,
) -> ConfigFile {
    let mut cfg = Config::disabled();

    cfg.plan_desktop = plan;
    cfg.debounce_seconds = debounce_seconds;
    cfg.notify_before_action = notify_before_action;

    ConfigFile {
        default: cfg,
        profiles: vec![],
        active_profile: None,
    }
}

fn step(kind: PlanStepKind, timeout: u64, cmd: &str) -> PlanStep {
    PlanStep {
        kind,
        timeout_seconds: timeout,
        command: Some(cmd.to_string()),
        resume_command: None,
        notification: None,
        notify_seconds_before: None,
    }
}

#[test]
fn per_step_timers_chain_from_previous_fire() {
    let plan = vec![
        step(PlanStepKind::Startup, 5, "a"),
        step(PlanStepKind::Dpms, 7, "b"),
    ];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 4000 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 5000 })
        .unwrap();
    assert_eq!(actions.len(), 1);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 11999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 12000 })
        .unwrap();
    assert_eq!(actions.len(), 1);
}

#[test]
fn skips_disabled_steps() {
    let mut disabled = step(PlanStepKind::Startup, 0, "nope");
    disabled.command = None;

    let plan = vec![disabled, step(PlanStepKind::Dpms, 1, "yes")];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1000 })
        .unwrap();
    assert_eq!(actions.len(), 1);
}

#[test]
fn lock_step_skipped_if_already_locked() {
    let plan = vec![
        step(PlanStepKind::LockScreen, 1, "lock"),
        step(PlanStepKind::Dpms, 1, "dpms"),
    ];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    state.set_locked(true);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1000 })
        .unwrap();

    assert_eq!(actions.len(), 1);
}

#[test]
fn activity_resets_cycle() {
    let plan = vec![
        step(PlanStepKind::Startup, 1, "a"),
        step(PlanStepKind::Dpms, 1, "b"),
    ];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);

    let _ = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1000 })
        .unwrap();
    assert_eq!(state.step_index(), 1);

    let _ = mgr
        .handle_event(
            &mut state,
            Event::UserActivity {
                kind: ActivityKind::Any,
                now_ms: 1500,
            },
        )
        .unwrap();

    assert_eq!(state.step_index(), 0);
    assert_eq!(state.step_base_ms(), 1500);
}

#[test]
fn notify_then_run_with_delay() {
    let mut s = step(PlanStepKind::Dpms, 5, "doit");
    s.notification = Some("warn".to_string());
    s.notify_seconds_before = Some(3);

    let mut mgr = Manager::new(cfg_with_plan_and_notify(vec![s], 2, true));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 6999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 7000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::Notify {
            message: "warn".to_string()
        }]
    );

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 9999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 10000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "doit".to_string()
        }]
    );
}

#[test]
fn late_tick_runs_notify_then_command_on_later_tick() {
    // With current semantics:
    // - Notify is emitted first (when we first observe we're past base_due).
    // - Run happens notify_seconds_before AFTER the notify emission time.
    //
    // debounce=1s, timeout=4s => base_due=5s
    // notify_seconds_before=2s
    // Late tick at 9000ms:
    // - Notify emitted at 9000ms
    // - Run due at 11000ms (9000 + 2000)

    let mut s = step(PlanStepKind::Startup, 4, "go");
    s.notification = Some("heads up".to_string());
    s.notify_seconds_before = Some(2);

    let mut mgr = Manager::new(cfg_with_plan_and_notify(vec![s], 1, true));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 9000 })
        .unwrap();

    assert_eq!(
        actions,
        vec![Action::Notify {
            message: "heads up".to_string()
        }]
    );

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 10999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 11000 })
        .unwrap();

    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "go".to_string()
        }]
    );
}

#[test]
fn no_notification_text_ignores_notify_seconds_before() {
    let mut s = step(PlanStepKind::Dpms, 5, "doit");
    s.notification = None;
    s.notify_seconds_before = Some(999);

    let mut mgr = Manager::new(cfg_with_plan_and_notify(vec![s], 2, true));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 6999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 7000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "doit".to_string()
        }]
    );
}
