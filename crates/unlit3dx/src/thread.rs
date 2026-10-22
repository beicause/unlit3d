//! World threads: one thread owning one `World`, with pacing and stop control.

use core::time::Duration;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use unlit_ecs::World;
use web_time::Instant;

use crate::executor::LocalTasks;
use crate::sync;

/// How long a dropped thread waits for its tick to notice the stop before it
/// is left detached instead of blocking the dropper.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// The stop signal a thread and its owner share.
#[derive(Debug, Default)]
struct Stop {
    /// Whether a stop has been requested.
    requested: Mutex<bool>,
    /// Signalled whenever `requested` turns true.
    signal: Condvar,
}

impl Stop {
    /// Request a stop and wake whoever is waiting on one.
    fn request(&self) {
        let mut requested = sync::lock(&self.requested);
        *requested = true;
        drop(requested);
        self.signal.notify_all();
    }

    /// Whether a stop has been requested.
    fn requested(&self) -> bool {
        *sync::lock(&self.requested)
    }
}

/// How a world's tick paces itself and observes a stop.
///
/// Every method may be called from inside the tick, which holds the control
/// by shared reference only: the stop lives in a shared flag, so a tick that
/// sleeps through the stop signal still sees it when it wakes.
pub struct ThreadControl {
    /// The stop signal the world's owner sends.
    stop: Arc<Stop>,
    /// Whether a thread drives the world.
    ///
    /// A world the host steps has no thread to sleep on: its waits return
    /// at once, and the host paces it by stepping it when a step is due.
    self_driven: bool,
}

impl ThreadControl {
    /// Whether a stop has been requested.
    ///
    /// Once true, stays true.
    pub fn stop_requested(&self) -> bool {
        self.stop.requested()
    }

    /// Whether a thread drives the world.
    ///
    /// A world the host steps is not self-driven: its waits return at once,
    /// because the host is what decides when a step is due.
    #[must_use]
    pub fn self_driven(&self) -> bool {
        self.self_driven
    }

    /// Sleep for at most `timeout`, returning early when a stop is requested.
    ///
    /// A world the host steps has no thread to sleep on, so this returns at
    /// once: the host owns the pacing, and one tick body then serves both
    /// modes unchanged.
    pub fn wait_timeout(&self, timeout: Duration) {
        if !self.self_driven {
            return;
        }
        let requested = sync::lock(&self.stop.requested);
        if *requested {
            return;
        }
        let _ = self.stop.signal.wait_timeout(requested, timeout);
    }
}

/// A cloneable view of a world thread's state.
///
/// A [`WorldThread`] owns the join handle and stops its thread on drop;
/// this shares only the flags, so a [`WorldHandle`](crate::handle::WorldHandle)
/// can report a threaded world's liveness and ask it to stop without owning
/// the thread.
#[derive(Clone)]
pub struct ThreadStatus {
    /// The stop signal, sent on drop.
    stop: Arc<Stop>,
    /// Whether the thread has been seen to exit.
    finished: Arc<AtomicBool>,
    /// The panic the thread died with, if it has died with one.
    panics: Arc<Mutex<Option<String>>>,
}

impl ThreadStatus {
    /// The panic the thread died with, if it has died with one.
    pub fn panic_message(&self) -> Option<String> {
        sync::lock(&self.panics).take()
    }

    /// Whether the thread has exited.
    ///
    /// A detached thread keeps reporting its own exit, so this stays
    /// meaningful after [`WorldThread::detach`].
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Ask the thread to stop, without waiting for it to finish.
    pub fn stop(&self) {
        self.stop.request();
    }

    /// Mark the world as finished.
    ///
    /// A world on its own thread marks itself when its loop ends; a stepped
    /// world's host calls this when its tick returns `false` or panics.
    pub(crate) fn finish(&self) {
        self.finished.store(true, Ordering::Release);
    }

    /// Record the panic that ended the world.
    pub(crate) fn record_panic(&self, message: String) {
        *sync::lock(&self.panics) = Some(message);
    }
}

/// One thread owning one `World`.
///
/// Dropping the thread asks it to stop and waits for it to finish, so a
/// threaded world's lifetime is exactly the proxy's. A tick that ignores the
/// stop cannot hang the dropper: after a short grace period the thread is
/// detached and the drop returns.
pub struct WorldThread {
    /// The shared state a handle reports.
    status: ThreadStatus,
    /// The thread handle, joined on drop.
    joined: Option<JoinHandle<()>>,
}

impl WorldThread {
    /// Spawn a thread that owns a fresh `World` and ticks it forever.
    ///
    /// The tick runs once per loop iteration and returns whether to keep
    /// running; a `false` return ends the thread as cleanly as a stop. A
    /// tick panic is caught, reported to the panic slot and logged, and
    /// ends the thread: a world whose tick has panicked is not worth
    /// restarting blindly.
    pub fn spawn<T>(name: &str, mut tick: T) -> Self
    where
        T: FnMut(&mut World, &ThreadControl) -> bool + Send + 'static,
    {
        Self::spawn_with(name, |_| (), move |world, control, ()| tick(world, control))
    }

    /// Like [`spawn`](Self::spawn), with a setup step and the state it
    /// produces.
    ///
    /// `init` runs once on the thread, against the fresh world, before the
    /// first job is drained; whatever it returns is handed to every tick.
    /// That ordering is what lets a bridge create the components its jobs
    /// assume exist - the physics world with its configured gravity, say -
    /// so a job sent immediately after spawning already finds them.
    pub fn spawn_with<S, I, T>(name: &str, init: I, mut tick: T) -> Self
    where
        I: FnOnce(&mut World) -> S + Send + 'static,
        T: FnMut(&mut World, &ThreadControl, &mut S) -> bool + Send + 'static,
    {
        let stop = Arc::new(Stop::default());
        let panics: Arc<Mutex<Option<String>>> = Arc::default();
        let finished = Arc::new(AtomicBool::new(false));
        let name = name.to_owned();
        let thread_stop = Arc::clone(&stop);
        let thread_panics = Arc::clone(&panics);
        let thread_finished = Arc::clone(&finished);
        let thread_name = name.clone();
        let joined = std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                let control = ThreadControl {
                    stop: thread_stop,
                    self_driven: true,
                };
                let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let mut world = World::new();
                    // The world's async tasks are its own from here on: the
                    // setup may spawn one, and every tick drives them.
                    LocalTasks::install(&mut world);
                    let mut state = init(&mut world);
                    while !control.stop_requested() {
                        LocalTasks::pump(&mut world);
                        if !tick(&mut world, &control, &mut state) {
                            break;
                        }
                    }
                }));
                if let Err(payload) = outcome {
                    let message = panic_message(&*payload);
                    log::error!("the world thread {thread_name} panicked: {message}");
                    *sync::lock(&thread_panics) = Some(message);
                }
                thread_finished.store(true, Ordering::Release);
            })
            .expect("the world thread spawns");
        Self {
            status: ThreadStatus {
                stop,
                finished,
                panics,
            },
            joined: Some(joined),
        }
    }

    /// A cloneable view of the thread's state, for handles that outlive the
    /// proxy entity.
    #[must_use]
    pub fn status(&self) -> ThreadStatus {
        self.status.clone()
    }

    /// The panic the thread died with, if it has died with one.
    pub fn panic_message(&self) -> Option<String> {
        self.status.panic_message()
    }

    /// Whether the thread has exited.
    ///
    /// A detached thread keeps reporting its own exit, so this stays
    /// meaningful after [`detach`](Self::detach).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.status.is_finished()
    }

    /// Ask the thread to stop, without waiting for it to finish.
    pub fn stop(&self) {
        self.status.stop();
    }

    /// Wait up to `timeout` for the thread to finish.
    ///
    /// Returns whether it finished in time. A thread that already finished,
    /// or was detached, counts as finished.
    pub fn join_timeout(&mut self, timeout: Duration) -> bool {
        let Some(joined) = self.joined.as_ref() else {
            return true;
        };
        let deadline = Instant::now() + timeout;
        while !joined.is_finished() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.join();
        true
    }

    /// Leave the thread running without ever joining it.
    ///
    /// The thread stops when it notices the stop, but nothing waits for it.
    pub fn detach(&mut self) {
        self.joined.take();
    }

    /// Join the thread, clearing the handle.
    fn join(&mut self) {
        if let Some(joined) = self.joined.take() {
            // A panic was already reported through the panic slot.
            let _ = joined.join();
        }
    }
}

impl Drop for WorldThread {
    fn drop(&mut self) {
        self.status.stop();
        if !self.join_timeout(STOP_TIMEOUT) {
            log::warn!("a world thread did not stop within {STOP_TIMEOUT:?}; leaving it detached");
            self.detach();
        }
    }
}

/// Paces a fixed-rate loop: sleeps until the next step boundary.
///
/// The boundary advances by exactly one step per wait, so a tick that
/// finishes early does not shift the schedule; a tick that overruns a whole
/// step resynchronizes to the present instead of catching up in a burst.
///
/// The clock paces a self-driven world; a world the host steps does not
/// need it, because the host decides when each step is due.
pub struct FixedClock {
    /// Seconds per step.
    step: Duration,
    /// When the next step is due.
    next: Instant,
}

impl FixedClock {
    /// A clock stepping every `step`, starting now.
    pub fn new(step: Duration) -> Self {
        Self {
            step,
            next: Instant::now() + step,
        }
    }

    /// Sleep until the next step is due - or a stop arrives, which the caller
    /// sees through [`ThreadControl::stop_requested`] - then move to the
    /// step after it.
    pub fn wait(&mut self, control: &ThreadControl) {
        let now = Instant::now();
        if now < self.next {
            control.wait_timeout(self.next - now);
        }
        self.next += self.step;
        let now = Instant::now();
        if self.next <= now {
            self.next = now + self.step;
        }
    }

    /// Seconds per step.
    pub fn step_seconds(&self) -> f32 {
        self.step.as_secs_f32()
    }
}

/// A control and status pair for a world the host steps.
///
/// Nothing is spawned here: the world has no thread, its waits return at
/// once, and the host steps it whenever a step is due.
pub(crate) fn stepped_control() -> (ThreadControl, ThreadStatus) {
    let stop = Arc::new(Stop::default());
    let status = ThreadStatus {
        stop: Arc::clone(&stop),
        finished: Arc::default(),
        panics: Arc::default(),
    };
    let control = ThreadControl {
        stop,
        self_driven: false,
    };
    (control, status)
}

/// The message a panic payload carries, whatever shape it arrived in.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "no panic message".to_owned()
    }
}

#[cfg(test)]
mod tests {
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
    fn a_false_tick_ends_the_thread() {
        let thread = WorldThread::spawn("test", |_, _| false);
        assert!(
            wait_until(|| thread.is_finished().then_some(())).is_some(),
            "the thread never finished"
        );
        assert!(thread.panic_message().is_none());
    }

    /// A tick panic is reported through the panic slot.
    #[test]
    fn a_tick_panic_is_reported() {
        let thread = WorldThread::spawn("test", |_, _| panic!("the tick died"));
        let message =
            wait_until(|| thread.panic_message()).expect("the tick never reported its panic");
        assert_eq!(message, "the tick died");
    }

    /// A panic in the setup step is reported like a tick panic.
    #[test]
    fn a_setup_panic_is_reported() {
        let thread = WorldThread::spawn_with(
            "test",
            |_| -> () { panic!("the setup died") },
            |_, _, ()| false,
        );
        let message =
            wait_until(|| thread.panic_message()).expect("the setup never reported its panic");
        assert_eq!(message, "the setup died");
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
            true
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
        let mut thread = WorldThread::spawn_with(
            "test",
            move |_| {
                saw.store(true, Ordering::SeqCst);
                7u32
            },
            move |_, _, state| {
                assert_eq!(*state, 7);
                false
            },
        );
        assert!(thread.join_timeout(Duration::from_secs(2)));
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
            false
        });
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

    /// A detached thread is never joined.
    #[test]
    fn a_detached_thread_is_forgotten() {
        let mut thread = WorldThread::spawn("test", |_, control| {
            control.wait_timeout(Duration::from_millis(5));
            true
        });
        thread.stop();
        assert!(thread.join_timeout(Duration::from_secs(2)));
        thread.detach();
        assert!(thread.is_finished());
    }
}
