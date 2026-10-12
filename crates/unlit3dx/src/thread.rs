//! World threads: one thread owning one `World`, with pacing and stop control.

use core::time::Duration;
use std::cell::Cell;
use std::panic::AssertUnwindSafe;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use unlit_ecs::World;
use web_time::Instant;

/// How a world thread's tick paces itself and observes a stop.
///
/// Every method may be called from inside the tick, which holds the control
/// by shared reference only: the seen-stop answer lives in a cell, so a tick
/// that sleeps through the stop signal still sees it when it wakes.
pub struct ThreadControl {
    /// The stop signal the thread's owner sends.
    stop: Receiver<()>,
    /// Whether a stop signal has already been observed.
    stop_seen: Cell<bool>,
}

impl ThreadControl {
    /// Whether a stop has been requested.
    ///
    /// Once true, stays true: the answer is remembered.
    pub fn stop_requested(&self) -> bool {
        if !self.stop_seen.get() && self.stop.try_recv().is_ok() {
            self.stop_seen.set(true);
        }
        self.stop_seen.get()
    }

    /// Sleep for at most `timeout`, returning early when a stop is requested.
    pub fn wait_timeout(&self, timeout: Duration) {
        if self.stop_requested() {
            return;
        }
        if self.stop.recv_timeout(timeout).is_ok() {
            self.stop_seen.set(true);
        }
    }
}

/// One thread owning one `World`.
///
/// Dropping the thread asks it to stop and waits for it to finish, so a
/// remote world's lifetime is exactly the proxy's.
pub struct WorldThread {
    /// The stop signal, sent on drop.
    stop: Sender<()>,
    /// The thread handle, joined on drop.
    joined: Option<JoinHandle<()>>,
    /// The panic the thread died with, if it has died with one.
    panics: Receiver<String>,
}

impl WorldThread {
    /// Spawn a thread that owns a fresh `World` and ticks it forever.
    ///
    /// The tick runs once per loop iteration and returns whether to keep
    /// running; a `false` return ends the thread as cleanly as a stop. A
    /// tick panic is caught, reported to the panic channel and logged, and
    /// ends the thread: a world whose tick has panicked is not worth
    /// restarting blindly.
    pub fn spawn<T>(name: &str, mut tick: T) -> Self
    where
        T: FnMut(&mut World, &ThreadControl) -> bool + Send + 'static,
    {
        let (stop, stop_receiver) = unbounded::<()>();
        let (panics, panic_receiver) = bounded(1);
        let name = name.to_owned();
        let joined = std::thread::Builder::new()
            .name(name.clone())
            .spawn(move || {
                let mut world = World::new();
                let control = ThreadControl {
                    stop: stop_receiver,
                    stop_seen: Cell::new(false),
                };
                loop {
                    if control.stop_requested() {
                        break;
                    }
                    match std::panic::catch_unwind(AssertUnwindSafe(|| tick(&mut world, &control)))
                    {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(payload) => {
                            let message = panic_message(&*payload);
                            log::error!("the world thread {} panicked: {message}", name.clone());
                            let _ = panics.try_send(message);
                            break;
                        }
                    }
                }
            })
            .expect("the world thread spawns");
        Self {
            stop,
            joined: Some(joined),
            panics: panic_receiver,
        }
    }

    /// The panic the thread died with, if it has died with one.
    pub fn panic_message(&self) -> Option<String> {
        self.panics.try_recv().ok()
    }

    /// Whether the thread has exited.
    pub fn is_finished(&self) -> bool {
        self.joined
            .as_ref()
            .is_some_and(|joined| joined.is_finished())
    }
}

impl Drop for WorldThread {
    fn drop(&mut self) {
        // Best effort: a thread already gone has nothing left to stop.
        let _ = self.stop.send(());
        if let Some(joined) = self.joined.take() {
            let _ = joined.join();
        }
    }
}

/// Paces a fixed-rate loop: sleeps until the next step boundary.
pub struct FixedClock {
    /// Seconds per step.
    step: Duration,
    /// When the current step began.
    last: Instant,
}

impl FixedClock {
    /// A clock stepping every `step`, starting now.
    pub fn new(step: Duration) -> Self {
        Self {
            step,
            last: Instant::now(),
        }
    }

    /// Sleep until the next step is due — or a stop arrives, which the caller
    /// sees through [`ThreadControl::stop_requested`] — then restart the step.
    pub fn wait(&mut self, control: &ThreadControl) {
        let remaining = self.step.saturating_sub(self.last.elapsed());
        if remaining > Duration::ZERO {
            control.wait_timeout(remaining);
        }
        self.last = Instant::now();
    }

    /// Seconds per step.
    pub fn step_seconds(&self) -> f32 {
        self.step.as_secs_f32()
    }
}

/// The message a panic payload carries, whatever shape it arrived in.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
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

    /// The thread stops when its tick asks it to, and the stop is observable.
    #[test]
    fn a_false_tick_ends_the_thread() {
        let thread = WorldThread::spawn("test", |_, _| false);
        let finished = (0..1_000).any(|_| {
            thread.is_finished() || {
                std::thread::sleep(Duration::from_millis(1));
                thread.is_finished()
            }
        });
        assert!(finished, "the thread never finished");
        assert!(thread.panic_message().is_none());
    }

    /// A tick panic is reported through the panic channel.
    #[test]
    fn a_tick_panic_is_reported() {
        let thread = WorldThread::spawn("test", |_, _| panic!("the tick died"));
        let message = loop {
            if let Some(message) = thread.panic_message() {
                break message;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(message, "the tick died");
    }

    /// A stop signalled while the tick sleeps wakes it up early.
    #[test]
    fn a_stop_wakes_a_sleeping_tick() {
        let wakeups = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = std::sync::Arc::clone(&wakeups);
        let thread = WorldThread::spawn("test", move |_, control| {
            let before = Instant::now();
            control.wait_timeout(Duration::from_millis(1_000));
            if before.elapsed() < Duration::from_millis(900) {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            true
        });
        // The first tick is asleep when the stop lands: the sleep ends early.
        std::thread::sleep(Duration::from_millis(10));
        drop(thread);
        assert!(
            wakeups.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the stop never woke the sleeping tick"
        );
    }
}
