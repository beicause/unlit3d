//! Requests a world answers, and the waiting for their answers.
//!
//! A job is one way: what one world says to another is a closure, and nothing
//! comes back with it. A caller that needs a value the other world makes asks
//! for it, and [@Pending] is the asking: it sends the job, names what it asked
//! for with a key of the caller's own — a ball's entity, say — and folds
//! whatever came back into the world on the caller's own tick.
//!
//! Three things make it more than a queue of answers:
//!
//! - **An answer is named.** Two requests are not told apart by the payload
//!   they carry: the key the caller asked with is the key the answer comes
//!   back under, so a ball finds its own instance however many others were
//!   asked for at the same time.
//! - **The wait is bounded.** A request whose answer has not come within
//!   `timeout` ticks of the world that asked is given up on, and so is every
//!   request still outstanding when the world that owes them ends: a job a
//!   panic took with it never answers, and neither does a world that stopped.
//! - **Nothing waits on an empty slot.** The answers are folded on the
//!   caller's tick, so a caller never blocks, and an answer that arrives for
//!   a request already given up on is dropped instead of delivered twice.
//!
//! A [@Pending] is not a future and the wait is not a wakeup: an answer lands
//! in a queue, and the tick that needs it folds it. That is what keeps a
//! stepped world's wait identical to a threaded one's, and what lets an answer
//! be applied straight to the world rather than handed to a continuation
//! outside it — a continuation could not hold the `&mut World` the answer is
//! for, and would have to wait for the tick anyway.

use async_channel::{Receiver, Sender, unbounded};

use unlit_ecs::World;

use crate::channel::SnapshotReader;
use crate::job::{JobSender, SendError};
use crate::thread::WorldState;

/// Whether the world behind a reader of its state has ended.
///
/// A question asked of a world that panicked is never answered, and neither is
/// one asked of a world that stopped: the job is gone with the turn it ran in.
/// A caller that asked waits on this instead of waiting forever.
fn ended(state: &SnapshotReader<WorldState>) -> bool {
    matches!(
        state.latest().as_deref(),
        Some(WorldState::Stopped | WorldState::Panicked(_))
    )
}

/// How one request ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled<T> {
    /// The world it was asked of answered, with this.
    ///
    /// A request the world could not carry out answers as well, with a value
    /// that says so: a show that did not happen answers `false` rather than
    /// nothing, so the caller learns the difference between a world that has
    /// not answered and one that answered no.
    Answered(T),
    /// The wait is over and nothing came: the request's ticks ran out, or the
    /// world that owed it ended.
    Expired,
}

/// The requests one world owes a caller, and the answers that have come back.
///
/// One [@Pending] serves one world: it is built with the state that world
/// publishes, and every request made through it goes to it. A caller that asks
/// two worlds for two things keeps two.
///
/// `K` is what the caller names a request by, and `T` what the world answers
/// with.
pub struct Pending<K, T> {
    /// What a job publishes its answer through. Cloned into every request.
    answerer: Sender<(K, T)>,
    /// Where the answers land, in the order they were published.
    answers: Receiver<(K, T)>,
    /// The state of the world that owes the answers.
    state: SnapshotReader<WorldState>,
    /// How many ticks a request may go unanswered; `None` waits for as long
    /// as the world owes it.
    timeout: Option<u32>,
    /// The requests still outstanding, and how many ticks each has left.
    outstanding: Vec<(K, Option<u32>)>,
}

impl<K, T> Pending<K, T>
where
    K: Eq + Clone + Send + 'static,
    T: Send + 'static,
{
    /// Requests for the world `state` reports on, each allowed to go
    /// unanswered for `timeout` ticks.
    ///
    /// A tick is one [@advance](Self::advance): whatever drives the asking
    /// world decides what one is worth, so a world stepped at a fixed rate
    /// measures its waits in steps and one paced by a clock measures them in
    /// frames of its own. `None` waits until the world answers or ends.
    #[must_use]
    pub fn new(state: SnapshotReader<WorldState>, timeout: Option<u32>) -> Self {
        let (answerer, answers) = unbounded();
        Self {
            answerer,
            answers,
            state,
            timeout,
            outstanding: Vec::new(),
        }
    }

    /// Ask the world behind `sender` for what `make` returns, as the request
    /// `key` names.
    ///
    /// `make` runs once, on the world's own thread, and whatever it returns is
    /// the answer `key` receives. The send is refused when that world's queue
    /// is full or it has stopped, and then no request is outstanding: the
    /// caller decides whether to ask again on a later tick.
    ///
    /// A request already outstanding under `key` is sent again without
    /// lengthening its wait: the answer already owed is the one that arrives,
    /// and the tick the first was asked on is the one the wait is measured
    /// from.
    pub fn request<F>(&mut self, sender: &JobSender, key: K, make: F) -> Result<(), SendError>
    where
        F: FnOnce(&mut World) -> T + Send + 'static,
    {
        let answerer = self.answerer.clone();
        let asked = key.clone();
        sender.send(Box::new(move |world: &mut World| {
            // The receiver outlives this job: the queue is unbounded and never
            // closes while a Pending holds it, and an answer nobody waits for
            // is dropped on the next advance rather than held.
            let _ = answerer.try_send((asked, make(world)));
        }))?;
        if !self.outstanding.iter().any(|(asked, _)| *asked == key) {
            self.outstanding.push((key, self.timeout));
        }
        Ok(())
    }

    /// What ended on this tick: the requests the world answered, and the ones
    /// given up on, each under the key it was asked with.
    ///
    /// Call it once per tick of the asking world, and take every item it
    /// writes: an unanswered request is given up on only once, and an answer
    /// is handed out only once. A request is expired when its ticks ran out,
    /// or at once when the world that owed it ended — a job that panicked
    /// takes its answer with it, and a world that stopped never runs another.
    ///
    /// An answer that arrived for a request no longer outstanding is dropped:
    /// the request was given up on, and whatever it asked for has already been
    /// dealt with. Requests come out oldest first — the answered in the order
    /// the world published them, then the expired in the order they were
    /// asked — so two runs of the same ticks settle the same requests in the
    /// same order.
    ///
    /// `settled` is cleared and reused, so a caller that keeps one allocates
    /// nothing per tick but what the answers themselves weigh.
    pub fn advance(&mut self, settled: &mut Vec<(K, Settled<T>)>) {
        settled.clear();
        while let Ok((key, value)) = self.answers.try_recv() {
            if self.take_outstanding(&key) {
                settled.push((key, Settled::Answered(value)));
            }
        }
        let ended = ended(&self.state);
        let mut outstanding = std::mem::take(&mut self.outstanding);
        outstanding.retain_mut(|(key, remaining)| {
            if ended {
                settled.push((key.clone(), Settled::Expired));
                return false;
            }
            let Some(left) = remaining else {
                return true;
            };
            *left = left.saturating_sub(1);
            if *left == 0 {
                settled.push((key.clone(), Settled::Expired));
                return false;
            }
            true
        });
        self.outstanding = outstanding;
    }

    /// Whether a request named `key` is still owed, taking it out if it is.
    fn take_outstanding(&mut self, key: &K) -> bool {
        let Some(at) = self.outstanding.iter().position(|(asked, _)| asked == key) else {
            return false;
        };
        self.outstanding.remove(at);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Snapshot;
    use crate::job::Job;

    /// Requests for a world in `state`, and the queue its requests go through.
    fn asking(
        state: WorldState,
        timeout: Option<u32>,
    ) -> (Pending<u32, u32>, JobSender, async_channel::Receiver<Job>) {
        let snapshot = Snapshot::new();
        snapshot.publish(state);
        let pending = Pending::new(snapshot.reader(), timeout);
        let (sender, receiver) = async_channel::unbounded();
        (pending, JobSender::new(sender), receiver)
    }

    /// Run every job queued so far.
    fn drain(receiver: &async_channel::Receiver<Job>, world: &mut World) {
        while let Ok(job) = receiver.try_recv() {
            job(world);
        }
    }

    /// What one advance settled.
    fn settled(pending: &mut Pending<u32, u32>) -> Vec<(u32, Settled<u32>)> {
        let mut settled = Vec::new();
        pending.advance(&mut settled);
        settled
    }

    /// An answer reaches the caller under the key it was asked with, however
    /// many were asked at once.
    #[test]
    fn an_answer_comes_back_under_its_key() {
        let (mut pending, jobs, receiver) = asking(WorldState::Running, None);
        let mut world = World::new();
        for key in 0..4u32 {
            pending
                .request(&jobs, key, move |_: &mut World| key * 10)
                .expect("the queue has room");
        }
        drain(&receiver, &mut world);

        assert_eq!(
            settled(&mut pending),
            vec![
                (0, Settled::Answered(0)),
                (1, Settled::Answered(10)),
                (2, Settled::Answered(20)),
                (3, Settled::Answered(30)),
            ]
        );
        assert!(
            settled(&mut pending).is_empty(),
            "an answer is handed out once"
        );
    }

    /// A request whose job never ran is given up on once its ticks ran out,
    /// and not before.
    #[test]
    fn a_request_is_given_up_on_after_its_ticks() {
        let (mut pending, jobs, _receiver) = asking(WorldState::Running, Some(2));
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");

        assert!(settled(&mut pending).is_empty(), "one tick of two is spent");
        assert_eq!(settled(&mut pending), vec![(1, Settled::Expired)]);
    }

    /// A world that ended owes nothing: every request still outstanding is
    /// given up on at once, without waiting for its ticks to run out.
    #[test]
    fn an_ended_world_answers_nothing() {
        let (mut pending, jobs, receiver) = asking(WorldState::Stopped, Some(100));
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");

        assert_eq!(
            settled(&mut pending),
            vec![(1, Settled::Expired)],
            "the job sat in the queue of a world that never ran it"
        );
        assert_eq!(receiver.len(), 1, "the job was still sent");
    }

    /// An answer the world published before it ended is still delivered: the
    /// world owed it and paid, and ending afterwards takes nothing back.
    #[test]
    fn an_answer_published_before_the_world_ended_is_delivered() {
        let (mut pending, jobs, receiver) = asking(WorldState::Stopped, Some(100));
        let mut world = World::new();
        pending
            .request(&jobs, 1, |_: &mut World| 7)
            .expect("the queue has room");
        drain(&receiver, &mut world);

        assert_eq!(settled(&mut pending), vec![(1, Settled::Answered(7))]);
    }

    /// An answer that arrived for a request already given up on is dropped
    /// rather than delivered late.
    #[test]
    fn an_answer_for_a_request_given_up_on_is_dropped() {
        let (mut pending, jobs, receiver) = asking(WorldState::Running, Some(1));
        let mut world = World::new();
        pending
            .request(&jobs, 1, |_: &mut World| 7)
            .expect("the queue has room");

        assert_eq!(settled(&mut pending), vec![(1, Settled::Expired)]);

        // The job runs on a drain after the wait is over, which is what a
        // world that was a tick behind looks like from here.
        drain(&receiver, &mut world);
        assert!(
            settled(&mut pending).is_empty(),
            "the late answer was dropped"
        );
    }

    /// A request refused by the world's queue was never asked: it waits for
    /// nothing and expires to nobody.
    #[test]
    fn a_refused_request_waits_for_nothing() {
        let (mut pending, jobs, receiver) = asking(WorldState::Running, Some(1));
        drop(receiver);
        assert_eq!(
            pending.request(&jobs, 1, |_: &mut World| 0).err(),
            Some(SendError::Closed)
        );

        assert!(settled(&mut pending).is_empty(), "nothing was ever asked");
    }

    /// Asking again under a key already outstanding does not lengthen its
    /// wait: the tick the first was asked on is the one that counts.
    #[test]
    fn asking_again_keeps_the_first_wait() {
        let (mut pending, jobs, _receiver) = asking(WorldState::Running, Some(2));
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");
        settled(&mut pending);
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");

        // One tick stood between the first ask and this one, so the wait the
        // first ask was given runs out here however recently it was repeated.
        assert_eq!(settled(&mut pending), vec![(1, Settled::Expired)]);
    }

    /// A request with no timeout waits as long as the world owes it.
    #[test]
    fn a_request_with_no_timeout_waits_until_the_world_ends() {
        let (mut pending, jobs, _receiver) = asking(WorldState::Running, None);
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");

        for _ in 0..10 {
            assert!(settled(&mut pending).is_empty(), "the wait has no end");
        }
    }
}
