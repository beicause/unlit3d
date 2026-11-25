use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// How long a test waits for a thread to reach the state it asserts on.
///
/// Generous, because the wait is not what is being measured: on a loaded
/// runner a thread can go a long while without being scheduled at all,
/// and a short budget would fail a thread that was merely slow to start.
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll until `ready` yields a value, or give up when the timeout runs
/// out.
///
/// Some of what a test waits for can only be read once - a panic message
/// is taken out of its slot - so the poll takes it rather than asking
/// twice. Returns `None` on giving up, so the caller asserts on it
/// instead of looping without end when a thread never gets to run.
fn wait_until<T>(ready: impl Fn() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Some(value) = ready() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The thread stops when its tick asks it to, and the stop is observable.
#[test]
fn a_finished_tick_ends_the_thread() {
    let thread =
        WorldThread::spawn("test", |_, _| TickOutcome::Finished).expect("the thread starts");
    let state = thread.state();
    assert!(
        wait_until(|| thread.is_finished().then_some(())).is_some(),
        "the thread never finished"
    );
    assert_eq!(state.latest().as_deref(), Some(&WorldState::Stopped));
}

/// A tick panic is reported through the state.
#[test]
fn a_tick_panic_is_reported() {
    let thread =
        WorldThread::spawn("test", |_, _| panic!("the tick died")).expect("the thread starts");
    let state = thread.state();
    assert!(
        wait_until(|| thread.is_finished().then_some(())).is_some(),
        "the thread never finished"
    );
    assert_eq!(
        state.latest().as_deref(),
        Some(&WorldState::Failed(crate::turn::WorldError::Panic {
            stage: crate::turn::WorldStage::Body,
            message: "the tick died".into(),
        }))
    );
}

/// A panic in the setup step is reported like a tick panic.
#[test]
fn a_setup_panic_is_reported() {
    let (_sender, inbox) = async_channel::bounded(1);
    let thread = WorldThread::spawn_with(
        "test",
        inbox,
        |_| -> () { panic!("the setup died") },
        |_, _, ()| TickOutcome::Finished,
    )
    .expect("the thread starts");
    let state = thread.state();
    assert!(
        wait_until(|| thread.is_finished().then_some(())).is_some(),
        "the thread never finished"
    );
    assert_eq!(
        state.latest().as_deref(),
        Some(&WorldState::Failed(crate::turn::WorldError::Panic {
            stage: crate::turn::WorldStage::Setup,
            message: "the setup died".into(),
        }))
    );
}

/// A stop ends the tick's wait early instead of sleeping it out.
#[test]
fn a_stop_wakes_a_sleeping_tick() {
    // The stop can only cut a wait short if a tick has begun one, so the
    // drop waits for the tick to say it is about to wait rather than
    // sleeping a while and hoping. A thread on a loaded runner has not
    // reached its first tick ten milliseconds in, and a stop that lands
    // before it does has nothing to wake.
    let waiting = Arc::new(AtomicBool::new(false));
    let about_to_wait = Arc::clone(&waiting);
    let wakeups = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&wakeups);
    let thread = WorldThread::spawn("test", move |_, control| {
        about_to_wait.store(true, Ordering::SeqCst);
        let before = Instant::now();
        control.wait_timeout(Duration::from_millis(1_000));
        if before.elapsed() < Duration::from_millis(900) {
            observed.fetch_add(1, Ordering::SeqCst);
        }
        TickOutcome::Running
    });
    let waited = wait_until(|| waiting.load(Ordering::SeqCst).then_some(()));
    drop(thread);
    assert!(waited.is_some(), "the tick never reached its wait");
    assert!(
        wakeups.load(Ordering::SeqCst) >= 1,
        "the stop never woke the sleeping tick"
    );
}

/// Setup runs before the first tick, and its state reaches every tick.
#[test]
fn setup_runs_before_the_first_tick() {
    let ran = Arc::new(AtomicBool::new(false));
    let saw = Arc::clone(&ran);
    let (_sender, inbox) = async_channel::bounded(1);
    let mut thread = WorldThread::spawn_with(
        "test",
        inbox,
        move |_| {
            saw.store(true, Ordering::SeqCst);
            7u32
        },
        move |_, _, state| {
            assert_eq!(*state, 7);
            TickOutcome::Finished
        },
    )
    .expect("the thread starts");
    assert_eq!(
        thread.join_timeout(Duration::from_secs(2)),
        JoinOutcome::Joined
    );
    assert!(ran.load(Ordering::SeqCst), "the setup never ran");
}

/// Dropping a thread whose tick ignores the stop returns instead of
/// hanging, and leaves the thread detached.
#[test]
fn a_stuck_tick_does_not_hang_the_drop() {
    let started = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&started);
    let thread = WorldThread::spawn("test", move |_, _| {
        flag.store(true, Ordering::SeqCst);
        // Ignore the stop forever: the drop must give up on it.
        std::thread::sleep(Duration::from_secs(30));
        TickOutcome::Finished
    })
    .expect("the thread starts");
    assert!(
        wait_until(|| started.load(Ordering::SeqCst).then_some(())).is_some(),
        "the tick never ran"
    );
    let before = Instant::now();
    drop(thread);
    assert!(
        before.elapsed() < STOP_TIMEOUT + Duration::from_secs(1),
        "the drop waited far longer than the grace period"
    );
}

/// Waiting for a thread that is still ticking gives up on the timeout
/// instead of waiting for the thread.
#[test]
fn a_join_timeout_gives_up_on_a_stuck_tick() {
    let mut thread = WorldThread::spawn("test", |_, _| {
        // Sleep through the stop: nothing ends the tick but the sleep.
        std::thread::sleep(Duration::from_secs(30));
        TickOutcome::Finished
    })
    .expect("the thread starts");
    let before = Instant::now();
    assert_eq!(
        thread.join_timeout(Duration::from_millis(50)),
        JoinOutcome::TimedOut
    );
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "the wait outlasted its timeout"
    );
    thread.detach();
}

/// A detached thread is never joined.
#[test]
fn a_detached_thread_is_forgotten() {
    let mut thread = WorldThread::spawn("test", |_, control| {
        control.wait_timeout(Duration::from_millis(5));
        TickOutcome::Running
    })
    .expect("the thread starts");
    thread.stop();
    assert_eq!(
        thread.join_timeout(Duration::from_secs(2)),
        JoinOutcome::Joined
    );
    thread.detach();
    assert!(thread.is_finished());
}

/// A detached thread cannot be waited for: the handle is gone, so a wait
/// says it was detached instead of claiming the thread has stopped.
#[test]
fn a_detached_thread_is_never_reported_as_joined() {
    let mut thread = WorldThread::spawn("test", |_, control| {
        control.wait_timeout(Duration::from_millis(5));
        TickOutcome::Running
    })
    .expect("the thread starts");
    thread.stop();
    thread.detach();
    assert_eq!(
        thread.join_timeout(Duration::from_secs(2)),
        JoinOutcome::Detached
    );
    // The answer does not change with a longer wait: nothing here can
    // observe the thread exit any more.
    assert_eq!(
        thread.join_timeout(Duration::from_secs(2)),
        JoinOutcome::Detached
    );
}

/// A second wait after a joined thread reports that the handle is spent
/// rather than waiting for a thread that is no longer there.
#[test]
fn a_second_wait_says_the_handle_is_spent() {
    let mut thread =
        WorldThread::spawn("test", |_, _| TickOutcome::Finished).expect("the thread starts");
    assert_eq!(
        thread.join_timeout(Duration::from_secs(2)),
        JoinOutcome::Joined
    );
    assert_eq!(
        thread.join_timeout(Duration::from_secs(2)),
        JoinOutcome::AlreadyJoined
    );
}

/// A wait that timed out keeps the handle, so the same thread can still
/// be waited for once it stops.
#[test]
fn a_timed_out_wait_can_be_taken_again() {
    let stop = Arc::new(AtomicBool::new(false));
    let watched = Arc::clone(&stop);
    let mut thread = WorldThread::spawn("test", move |_, _| {
        if watched.load(Ordering::SeqCst) {
            TickOutcome::Finished
        } else {
            TickOutcome::Running
        }
    })
    .expect("the thread starts");
    assert_eq!(
        thread.join_timeout(Duration::from_millis(10)),
        JoinOutcome::TimedOut
    );
    stop.store(true, Ordering::SeqCst);
    assert_eq!(
        thread.join_timeout(Duration::from_secs(2)),
        JoinOutcome::Joined
    );
}

/// The step the fixed-step tests accumulate debt against.
const STEP: f32 = 1.0 / 60.0;

/// One step of time runs exactly one step and leaves nothing owed.
#[test]
fn a_step_of_time_runs_one_fixed_step() {
    let mut debt = 0.0;
    assert_eq!(fixed_steps(&mut debt, STEP, STEP), 1);
    assert_eq!(debt, 0.0);
}

/// The fraction of a step a tick did not spend waits for the next one
/// instead of being rounded away.
#[test]
fn an_unspent_fraction_waits_for_the_next_tick() {
    let mut debt = 0.0;
    assert_eq!(fixed_steps(&mut debt, STEP * 0.5, STEP), 0);
    assert!((debt - STEP * 0.5).abs() < STEP * 1e-4);
    assert_eq!(fixed_steps(&mut debt, STEP * 0.6, STEP), 1);
    assert!((debt - STEP * 0.1).abs() < STEP * 1e-4);
}

/// A stall past the catch-up limit runs the limit and drops the rest, so
/// a world that was paused resumes instead of replaying the pause.
#[test]
fn debt_past_the_catch_up_limit_is_discarded() {
    let mut debt = 0.0;
    assert_eq!(
        fixed_steps(&mut debt, STEP * 100.0, STEP),
        MAX_CATCH_UP_STEPS
    );
    assert_eq!(debt, 0.0);
    // Just short of the limit still runs every whole step it covers.
    assert_eq!(fixed_steps(&mut debt, STEP * 3.5, STEP), 3);
}

/// A clock that went backwards owes nothing.
#[test]
fn a_negative_interval_adds_no_time() {
    let mut debt = 0.0;
    assert_eq!(fixed_steps(&mut debt, -1.0, STEP), 0);
    assert_eq!(debt, 0.0);
}
