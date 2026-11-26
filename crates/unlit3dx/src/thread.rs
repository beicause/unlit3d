//! World threads: one thread owning one `World`, with pacing and stop control.

use core::time::Duration;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use async_channel::Receiver;
use unlit_ecs::sync;
use unlit_ecs::{World, bundle};
use unlit3d::frame::FrameClock;
use web_time::Instant;

use crate::channel::{Snapshot, SnapshotReader};
use crate::executor::LocalTasks;
use crate::job::Job;
use crate::time::{FrameDelta, SimulationDelta};
use crate::turn::{Link, StepOutcome, TickOutcome, WorldError, WorldStage, close_inbox, run_turn};

/// How long a dropped thread waits for its tick to notice the stop before it
/// is left detached instead of blocking the dropper.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// The stop signal a thread and its owner share.
#[derive(Debug, Default)]
pub(crate) struct Stop {
    /// Whether a stop has been requested, and whether the thread is over.
    ///
    /// Both are facts about the same thread and are waited on by the same
    /// callers, so one lock and one condition variable carry them: a waiter
    /// that takes the lock, finds nothing and waits holds the lock across the
    /// wait, so it cannot miss a signal sent before it went to sleep.
    requested: Mutex<StopState>,
    /// Signalled whenever `requested` or `finished` turns true.
    signal: Condvar,
}

/// What a stop signal has been told, and what it can tell.
#[derive(Debug, Default)]
struct StopState {
    /// Whether a stop has been requested.
    requested: bool,
    /// Whether the thread that owns this stop has left its loop.
    finished: bool,
}

impl Stop {
    /// Request a stop and wake whoever is waiting on one.
    fn request(&self) {
        let mut state = sync::lock(&self.requested);
        state.requested = true;
        drop(state);
        self.signal.notify_all();
    }

    /// Mark the thread as over and wake whoever is waiting on it.
    fn finish(&self) {
        let mut state = sync::lock(&self.requested);
        state.finished = true;
        drop(state);
        self.signal.notify_all();
    }

    /// Wait up to `timeout` for the thread to finish.
    ///
    /// Returns whether it finished in time. A thread that already finished
    /// returns at once, and a signal that arrives before the wait does not
    /// get missed: the check and the wait happen under one lock.
    fn wait_finished(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = sync::lock(&self.requested);
        loop {
            if state.finished {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = match self.signal.wait_timeout(state, deadline - now) {
                Ok((state, _)) => state,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// Whether a stop has been requested.
    fn requested(&self) -> bool {
        sync::lock(&self.requested).requested
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
    /// When the previous tick started, for a self-driven world's elapsed time.
    last_tick: Mutex<Option<Instant>>,
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

    /// Seconds since the previous tick, measured once per turn.
    pub(crate) fn frame_delta(&self) -> FrameDelta {
        let now = Instant::now();
        let mut last = sync::lock(&self.last_tick);
        let elapsed = last.map_or(0.0, |last| now.duration_since(last).as_secs_f64());
        *last = Some(now);
        FrameDelta::from_secs(elapsed)
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
        let state = sync::lock(&self.stop.requested);
        if state.requested {
            return;
        }
        let _ = self.stop.signal.wait_timeout(state, timeout);
    }
}

/// What a spawned world is doing.
///
/// A world runs until it stops or dies, and the state it published last is
/// the one a reader finds: a reader that arrives after the end still learns
/// how the world ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldState {
    /// The world is being driven.
    Running,
    /// The world ended: its tick asked to stop, or its owner stopped it.
    Stopped,
    /// The world failed: a job, a tick, a queued command or a collector
    /// panicked, or a queued command could not be applied, and this is the
    /// cause it carried.
    Failed(WorldError),
}

/// A cloneable view of a world's state.
///
/// A [`WorldThread`] owns the thread and stops it on drop; this shares the
/// stop signal, the state and the join handle, so a
/// [`WorldEndpoint`](crate::endpoint::WorldEndpoint) can report a world's
/// state, ask it to stop and wait for its thread without owning either. A
/// caller reads that state through the endpoint, so this stays an
/// implementation detail of the spawn functions.
#[derive(Clone)]
pub(crate) struct ThreadStatus {
    /// The stop signal, sent on drop.
    stop: Arc<Stop>,
    /// What the world is doing, and how it ended.
    state: Snapshot<WorldState>,
    /// This status's own reading of the state, for the tests that ask it
    /// directly instead of taking a reader.
    own: SnapshotReader<WorldState>,
    /// The thread to reclaim, shared with the handle that owns the world.
    ///
    /// `None` for a world no thread ever drove: a sub-world and a stepped
    /// world are turned by their owner, so there is nothing to wait for and
    /// a wait has to say so rather than report an end it never saw.
    join: Option<Arc<Mutex<JoinSlot>>>,
}

/// What is left of a thread's join handle.
#[derive(Debug, Default)]
struct JoinSlot {
    /// The handle, until someone joins it.
    handle: Option<JoinHandle<()>>,
    /// Whether the caller gave up the wait for good.
    detached: bool,
}

impl ThreadStatus {
    /// A status for a world a thread drives, whose handle arrives later.
    fn threaded(stop: Arc<Stop>) -> Self {
        Self::at(stop, Some(Arc::default()))
    }

    /// A status for a world the host turns, sharing the stop signal it holds.
    ///
    /// There is no thread to reclaim, so a wait answers
    /// [`JoinOutcome::NoHandle`]; the stop signal is the host's, so asking the
    /// world to stop still reaches the loop that turns it.
    pub(crate) fn host_owned(stop: Arc<Stop>) -> Self {
        Self::at(stop, None)
    }

    /// A status for a world that shares its owner's thread.
    ///
    /// Such a world - a sub-world on the host, a runner of the caller's own -
    /// ends when its owner says so, so the state stays
    /// [`WorldState::Running`] until then, and the stop signal belongs to
    /// that owner alone.
    pub(crate) fn detached() -> Self {
        Self::at(Arc::default(), None)
    }

    fn at(stop: Arc<Stop>, join: Option<Arc<Mutex<JoinSlot>>>) -> Self {
        let state = Snapshot::new();
        let own = state.reader();
        state.publish(WorldState::Running);
        Self {
            stop,
            state,
            own,
            join,
        }
    }

    /// A reader of the state this world publishes.
    ///
    /// Every reader sees the newest state, so one that arrives late still
    /// learns how the world ended.
    #[must_use]
    pub(crate) fn state(&self) -> SnapshotReader<WorldState> {
        self.state.reader()
    }

    /// Whether the world has stopped or died.
    ///
    /// A detached thread keeps reporting its own end, so this stays
    /// meaningful after [`WorldThread::detach`]. A world is published as
    /// [`WorldState::Running`] before it is reachable at all, so only the two
    /// ending states answer `true`; a world that has published nothing is not
    /// one this can be asked about.
    #[must_use]
    pub(crate) fn is_finished(&self) -> bool {
        matches!(
            self.current().as_deref(),
            Some(WorldState::Stopped | WorldState::Failed(_))
        )
    }

    pub(crate) fn current(&self) -> Option<Arc<WorldState>> {
        self.own.latest()
    }

    /// Ask the world to stop, without waiting for it to finish.
    pub(crate) fn stop(&self) {
        self.stop.request();
    }

    /// Whether the world has been asked to stop.
    ///
    /// A world on its own thread reads this from its tick loop; a world the
    /// host drives reads it when the host drains it.
    pub(crate) fn stop_requested(&self) -> bool {
        self.stop.requested()
    }

    /// Mark the world as finished.
    ///
    /// A world on its own thread marks itself when its loop ends; a stepped
    /// world's host calls this when its tick returns `false`, and a world
    /// dropped by its owner marks itself as it goes. A world that has already
    /// ended keeps the state it ended with: a panic stays the last word.
    pub(crate) fn finish(&self) {
        if self.is_finished() {
            return;
        }
        self.state.publish(WorldState::Stopped);
    }

    /// Record the failure that ended the world, cause and all.
    ///
    /// A failure is the last word: a world that failed reads as failed even
    /// after the driver that noticed it marks the world finished.
    pub(crate) fn record_failure(&self, error: WorldError) {
        self.state.publish(WorldState::Failed(error));
    }

    /// Wait up to `timeout` for the thread that drives this world to finish.
    ///
    /// A world no thread drove answers [`JoinOutcome::NoHandle`]: the caller
    /// learns there is nothing to reclaim instead of a wait that a detach
    /// would otherwise disguise as an end.
    pub(crate) fn join_timeout(&self, timeout: Duration) -> JoinOutcome {
        let Some(join) = self.join.as_deref() else {
            return JoinOutcome::NoHandle;
        };
        {
            let slot = sync::lock(join);
            if slot.detached {
                return JoinOutcome::Detached;
            }
            if slot.handle.is_none() {
                return JoinOutcome::AlreadyJoined;
            }
        }
        // The wait happens without the slot locked: the thread reaching its
        // end has no business waiting for a reader of the slot.
        if !self.stop.wait_finished(timeout) {
            return JoinOutcome::TimedOut;
        }
        let mut slot = sync::lock(join);
        if let Some(handle) = slot.handle.take() {
            // A panic was already reported through the world's state.
            let _ = handle.join();
        }
        JoinOutcome::Joined
    }

    /// Leave the thread running without ever joining it.
    pub(crate) fn detach(&self) {
        let Some(join) = self.join.as_deref() else {
            return;
        };
        let mut slot = sync::lock(join);
        slot.detached = true;
        slot.handle.take();
    }
}

/// Why a world thread could not be started.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ThreadError {
    /// The world's setup panicked, so there is no world to drive.
    ///
    /// A stepped world is set up on the caller's thread, before anything is
    /// spawned, so a failed setup leaves nothing behind to clean up.
    #[error("the {name} world could not be set up: {source}")]
    Setup {
        /// The name the world was to be known by.
        name: &'static str,
        /// Why the setup failed.
        #[source]
        source: WorldError,
    },
    /// The operating system refused to create the thread.
    #[error("the thread for the {name} world was not created: {source}")]
    Spawn {
        /// The name the thread was to run under.
        name: String,
        /// What the operating system reported.
        #[source]
        source: std::io::Error,
    },
}

/// What became of a thread someone waited for.
///
/// Whether a thread has been reclaimed and why its world ended are two
/// questions with two answers: this one says what the handle did, and
/// [`WorldState`] says how the world finished. A timed-out wait keeps the
/// handle, so a later call can wait again instead of reporting an end it
/// never saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum JoinOutcome {
    /// The thread finished, and its handle was reclaimed.
    Joined,
    /// The handle was detached earlier, so nothing here proves the thread
    /// has stopped: it may still be running.
    Detached,
    /// The thread had not finished when the timeout passed.
    TimedOut,
    /// The thread was joined by an earlier wait.
    AlreadyJoined,
    /// No thread ever drove this world, so there is nothing to reclaim.
    ///
    /// A sub-world and a stepped world are turned by their owner: their end is
    /// reported by [`WorldState`], not by a wait.
    NoHandle,
}

/// One thread owning one `World`.
///
/// Dropping the driver requests a cooperative stop and waits for up to two
/// seconds. If a job or tick does not return in time, the thread is detached
/// and may outlive its proxy. Call [`stop`](Self::stop) and then
/// [`join_timeout`](Self::join_timeout) when teardown must be confirmed.
pub struct WorldThread {
    /// The shared state a handle reports, and the shared join slot.
    status: ThreadStatus,
}

impl WorldThread {
    /// Spawn a thread that owns a fresh `World` and ticks it forever.
    ///
    /// The tick runs once per turn and returns whether to keep running; a
    /// `false` return ends the thread as cleanly as a stop. Nothing posts
    /// jobs to a world spawned this way, so the turn only runs the tick;
    /// [`spawn_with`](Self::spawn_with) is the one that takes a queue.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadError::Spawn`] when the operating system refuses to
    /// create the thread, so a caller that cannot run a world says so instead
    /// of unwinding.
    pub fn spawn<T>(name: &str, mut tick: T) -> Result<Self, ThreadError>
    where
        T: FnMut(&mut World, &ThreadControl) -> TickOutcome + Send + 'static,
    {
        // The queue is closed from the start: no producer holds its sender,
        // so a turn finds nothing queued and the tick alone drives the world.
        let (_sender, inbox) = async_channel::bounded(1);
        Self::spawn_with(
            name,
            inbox,
            |_| (),
            move |world, control, ()| tick(world, control),
        )
    }

    /// Like [`spawn`](Self::spawn), with the queue jobs arrive in and a setup
    /// step whose state every tick sees.
    ///
    /// `init` runs once on the thread, against the fresh world, before the
    /// first turn; whatever it returns is handed to every tick. That ordering
    /// is what lets a bridge create the components its jobs assume exist - the
    /// physics world with its configured gravity, say - so a job sent
    /// immediately after spawning already finds them.
    ///
    /// The loop runs one turn per iteration, the same turn a stepped
    /// world and a sub-world run, so the order in which jobs, the tick and the
    /// commit happen cannot differ between drivers.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadError::Spawn`] when the operating system refuses to
    /// create the thread.
    pub fn spawn_with<S, I, T>(
        name: &str,
        inbox: Receiver<Job>,
        init: I,
        mut tick: T,
    ) -> Result<Self, ThreadError>
    where
        I: FnOnce(&mut World) -> S + Send + 'static,
        T: FnMut(&mut World, &ThreadControl, &mut S) -> TickOutcome + Send + 'static,
    {
        let stop = Arc::new(Stop::default());
        let status = ThreadStatus::threaded(Arc::clone(&stop));
        let thread_status = status.clone();
        let thread_stop = Arc::clone(&stop);
        let thread_name = name.to_owned();
        let builder_name = thread_name.clone();
        let error_name = thread_name.clone();
        let joined = std::thread::Builder::new()
            .name(builder_name)
            .spawn(move || {
                let control = ThreadControl {
                    stop: Arc::clone(&thread_stop),
                    self_driven: true,
                    last_tick: Mutex::new(None),
                };
                let mut world = World::new();
                // A world is born with its frame state, so a behaviour
                // subscribed to the first turn already reads a clock rather
                // than finding none.
                world
                    .root_scope_mut()
                    .spawn_named("frame_clock", bundle![FrameClock::default()]);
                // The world's async tasks are its own from here on: the setup
                // may spawn one, and every turn drives them.
                LocalTasks::install(&mut world);
                // The setup is the one stage no turn runs, so it is contained
                // here rather than left to reach the thread and unwind it.
                let setup = std::panic::catch_unwind(AssertUnwindSafe(|| init(&mut world)));
                let mut state = match setup {
                    Ok(state) => state,
                    Err(payload) => {
                        let error = WorldError::Panic {
                            stage: WorldStage::Setup,
                            message: panic_message(payload.as_ref()).into(),
                        };
                        log::error!("the world thread {thread_name} failed: {error}");
                        thread_status.record_failure(error);
                        close_inbox(&inbox);
                        thread_stop.finish();
                        return;
                    }
                };
                let link = Link {
                    name: &thread_name,
                    inbox: &inbox,
                    status: &thread_status,
                };
                // One turn per iteration, the same one a stepped world and a
                // sub-world run: the queued jobs, the tick, then the commit
                // and the publish. Any outcome but `Ran` ends the loop, and
                // the turn has already recorded why.
                while run_turn(&mut world, link, |world| {
                    LocalTasks::pump(world);
                    tick(world, &control, &mut state)
                }) == StepOutcome::Ran
                {}
                // The state is the last word on how the world ended, so it is
                // published before whoever waits on the thread is woken.
                thread_stop.finish();
            })
            .map_err(|source| ThreadError::Spawn {
                name: error_name,
                source,
            })?;
        // The handle is shared with every endpoint that reaches this world, so
        // a caller that waits reclaims the thread exactly once no matter which
        // clone of the endpoint it waits through.
        if let Some(slot) = status.join.as_deref() {
            sync::lock(slot).handle = Some(joined);
        }
        Ok(Self { status })
    }

    /// A cloneable view of the thread's state, for the handle the spawn hands
    /// back.
    #[must_use]
    pub(crate) fn status(&self) -> ThreadStatus {
        self.status.clone()
    }

    /// A reader of the state the world publishes.
    #[must_use]
    pub fn state(&self) -> SnapshotReader<WorldState> {
        self.status.state()
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
    /// The wait is woken by the thread itself as it leaves its loop, so it
    /// costs nothing while the thread runs. The answer says what became of the
    /// thread, not how the world ended: [`WorldState`] holds that. A wait that
    /// times out keeps the handle, so calling this again waits for the same
    /// thread instead of pretending it finished.
    pub fn join_timeout(&mut self, timeout: Duration) -> JoinOutcome {
        self.status.join_timeout(timeout)
    }

    /// Leave the thread running without ever joining it.
    ///
    /// The thread stops when it notices the stop, but nothing waits for it.
    /// Once detached, the handle can no longer report whether the thread has
    /// exited: [`join_timeout`](Self::join_timeout) answers
    /// [`JoinOutcome::Detached`] from then on, and
    /// [`is_finished`](Self::is_finished) keeps reporting the state the world
    /// publishes rather than the state of the thread.
    pub fn detach(&mut self) {
        self.status.detach();
    }
}

impl Drop for WorldThread {
    fn drop(&mut self) {
        self.status.stop();
        if self.status.join_timeout(STOP_TIMEOUT) == JoinOutcome::TimedOut {
            log::warn!("a world thread did not stop within {STOP_TIMEOUT:?}; leaving it detached");
            self.status.detach();
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

    /// Change how long a step lasts, keeping the schedule where it is.
    ///
    /// The next step is due one new step from now when the current one has
    /// already passed, so a shortened step never asks for a burst of waits
    /// to catch up.
    pub fn set_step(&mut self, step: Duration) {
        self.step = step;
        let now = Instant::now();
        if self.next <= now {
            self.next = now + step;
        }
    }
}

/// The most fixed steps one tick may run to catch up with the time that
/// passed.
pub const MAX_CATCH_UP_STEPS: u32 = 4;

/// Turn elapsed time into a whole number of fixed steps, carrying the rest
/// into the next tick as debt.
///
/// `debt` is what earlier ticks owed and did not spend; the returned count
/// is how many steps the accumulated time now covers, and `debt` keeps the
/// fraction of a step left over. Debt past [`MAX_CATCH_UP_STEPS`] steps is
/// discarded rather than owed: a world that stalled for a long while - a
/// suspended process, a debugger pause - would otherwise run a burst of
/// steps to catch up, and simulating a pause that long is neither cheap nor
/// wanted.
///
/// A zero `period` runs no steps and leaves the debt unchanged.
pub fn fixed_steps(
    debt: &mut SimulationDelta,
    delta: SimulationDelta,
    period: SimulationDelta,
) -> u32 {
    let period = f64::from(period.as_secs());
    if period == 0.0 {
        return 0;
    }
    let available = (f64::from(debt.as_secs()) + f64::from(delta.as_secs()))
        .min(period * f64::from(MAX_CATCH_UP_STEPS));
    let steps = (available / period) as u32;
    *debt = SimulationDelta::from_secs((available - f64::from(steps) * period) as f32);
    steps
}

/// A control and status pair for a world the host steps.
///
/// Nothing is spawned here: the world has no thread, its waits return at
/// once, and the host steps it whenever a step is due.
pub(crate) fn stepped_control() -> (ThreadControl, ThreadStatus) {
    let stop = Arc::new(Stop::default());
    // No thread turns this world, so a wait has nothing to reclaim; the stop
    // signal is still the one the host's turn reads.
    let status = ThreadStatus::host_owned(Arc::clone(&stop));
    let control = ThreadControl {
        stop,
        self_driven: false,
        last_tick: Mutex::new(None),
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
mod tests;
