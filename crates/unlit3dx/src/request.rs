//! Requests a world answers, and the waiting for their answers.
//!
//! A job is one way: what one world says to another is a closure, and nothing
//! comes back with it. A caller that needs a value the other world makes asks
//! for it, and [Pending](crate::request::Pending) is the asking: it sends the job, names what it asked
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
//!   `timeout` seconds is given up on, and so is every
//!   request still outstanding when the world that owes them ends: a job a
//!   panic took with it never answers, and neither does a world that stopped.
//! - **Nothing waits on an empty slot.** The answers are folded on the
//!   caller's tick, so a caller never blocks, and an answer that arrives for
//!   a request already given up on is dropped instead of delivered twice.
//!
//! A [Pending](crate::request::Pending) is not a future and the wait is not a wakeup: an answer lands
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
/// A question asked of a world that failed is never answered, and neither is
/// one asked of a world that stopped: the job is gone with the turn it ran in.
/// A caller that asked waits on this instead of waiting forever.
fn ended(state: &SnapshotReader<WorldState>) -> bool {
    matches!(
        state.latest().as_deref(),
        Some(WorldState::Stopped | WorldState::Failed(_))
    )
}

/// What a request did when it was asked.
///
/// The two answers are not the same event: `Queued` is a job on its way to the
/// world that will answer it, and `Coalesced` is a request the caller already
/// has outstanding, whose `make` never ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Asked {
    /// The job was queued: the world will answer unless it stops first.
    Queued,
    /// A request under this key was already outstanding, so nothing was
    /// queued and the `make` of this one was dropped unrun.
    Coalesced,
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
    /// The wait is over and nothing came: the request's time ran out, or the
    /// world that owed it ended.
    Expired,
}

type RequestId = u64;

struct Outstanding<K> {
    key: K,
    id: RequestId,
    remaining: Option<f32>,
}

/// The requests one world owes a caller, and the answers that have come back.
///
/// One [Pending] serves one world: it is built with the state that world
/// publishes, and every request made through it goes to it. A caller that asks
/// two worlds for two things keeps two.
///
/// `K` is what the caller names a request by, and `T` what the world answers
/// with.
pub struct Pending<K, T> {
    answerer: Sender<(RequestId, T)>,
    answers: Receiver<(RequestId, T)>,
    next_id: RequestId,
    /// The state of the world that owes the answers.
    state: SnapshotReader<WorldState>,
    /// How many seconds a request may go unanswered; `None` waits for as long
    /// as the world owes it.
    timeout: Option<f32>,
    /// The requests still outstanding, and how many seconds each has left.
    outstanding: Vec<Outstanding<K>>,
}

impl<K, T> Pending<K, T>
where
    K: Eq + Clone + Send + 'static,
    T: Send + 'static,
{
    /// Requests for the world `state` reports on, each allowed to go
    /// unanswered for `timeout` seconds. `None` waits until the world answers
    /// or ends.
    #[must_use]
    pub fn new(state: SnapshotReader<WorldState>, timeout: Option<f32>) -> Self {
        let (answerer, answers) = unbounded();
        Self {
            answerer,
            answers,
            next_id: 0,
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
    /// caller decides whether to ask again on a later tick, and `make` goes
    /// with the refused job.
    ///
    /// A request already outstanding under `key` is coalesced, and the answer
    /// says so: no second job is queued, `make` is dropped unrun, and the first
    /// request keeps ownership of the answer and of the time it was given. A
    /// caller that asks once per tick therefore learns that the tick's work was
    /// not done rather than taking the silence for a lost registration.
    pub fn request<F>(&mut self, sender: &JobSender, key: K, make: F) -> Result<Asked, SendError>
    where
        F: FnOnce(&mut World) -> T + Send + 'static,
    {
        if self.outstanding.iter().any(|request| request.key == key) {
            return Ok(Asked::Coalesced);
        }
        let answerer = self.answerer.clone();
        let id = self.next_id;
        sender.send(Box::new(move |world: &mut World| {
            let _ = answerer.try_send((id, make(world)));
        }))?;
        self.next_id += 1;
        self.outstanding.push(Outstanding {
            key,
            id,
            remaining: self.timeout,
        });
        Ok(Asked::Queued)
    }

    /// What ended on this tick: the requests the world answered, and the ones
    /// given up on, each under the key it was asked with.
    ///
    /// Call it once per tick of the asking world, and take every item it
    /// writes: an unanswered request is given up on only once, and an answer
    /// is handed out only once. A request is expired when the time it was
    /// given runs out, or at once when the world that owed it ended — a job
    /// that panicked takes its answer with it, and a world that stopped never
    /// runs another.
    ///
    /// An answer that arrived for a request no longer outstanding is dropped:
    /// the request was given up on, and whatever it asked for has already been
    /// dealt with. Requests come out oldest first — the answered in the order
    /// the world published them, then the expired in the order they were
    /// asked — so two runs of the same durations settle the same requests in
    /// the same order.
    ///
    /// `settled` is cleared and reused, so a caller that keeps one allocates
    /// nothing per tick but what the answers themselves weigh.
    pub fn advance(&mut self, delta_seconds: f32, settled: &mut Vec<(K, Settled<T>)>) {
        settled.clear();
        while let Ok((id, value)) = self.answers.try_recv() {
            if let Some(at) = self.outstanding.iter().position(|request| request.id == id) {
                let request = self.outstanding.remove(at);
                settled.push((request.key, Settled::Answered(value)));
            }
        }
        let ended = ended(&self.state);
        let mut outstanding = std::mem::take(&mut self.outstanding);
        outstanding.retain_mut(|Outstanding { key, remaining, .. }| {
            if ended {
                settled.push((key.clone(), Settled::Expired));
                return false;
            }
            let Some(left) = remaining else {
                return true;
            };
            *left -= delta_seconds.max(0.0);
            if *left <= 0.0 {
                settled.push((key.clone(), Settled::Expired));
                return false;
            }
            true
        });
        self.outstanding = outstanding;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Snapshot;
    use crate::job::{Job, SendErrorKind};

    /// Requests for a world in `state`, and the queue its requests go through.
    fn asking(
        state: WorldState,
        timeout: Option<f32>,
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

    /// What one advance of `delta_seconds` settled.
    fn settled(pending: &mut Pending<u32, u32>, delta_seconds: f32) -> Vec<(u32, Settled<u32>)> {
        let mut settled = Vec::new();
        pending.advance(delta_seconds, &mut settled);
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
            settled(&mut pending, 1.0),
            vec![
                (0, Settled::Answered(0)),
                (1, Settled::Answered(10)),
                (2, Settled::Answered(20)),
                (3, Settled::Answered(30)),
            ]
        );
        assert!(
            settled(&mut pending, 1.0).is_empty(),
            "an answer is handed out once"
        );
    }

    /// A request whose job never ran is given up on once its time runs out,
    /// and not before.
    #[test]
    fn a_request_is_given_up_on_after_its_time_runs_out() {
        let (mut pending, jobs, _receiver) = asking(WorldState::Running, Some(2.0));
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");

        assert!(
            settled(&mut pending, 1.0).is_empty(),
            "one tick of two is spent"
        );
        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::Expired)]);
    }

    /// A world that ended owes nothing: every request still outstanding is
    /// given up on at once, without waiting for its time to run out.
    #[test]
    fn an_ended_world_answers_nothing() {
        let (mut pending, jobs, receiver) = asking(WorldState::Stopped, Some(100.0));
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");

        assert_eq!(
            settled(&mut pending, 1.0),
            vec![(1, Settled::Expired)],
            "the job sat in the queue of a world that never ran it"
        );
        assert_eq!(receiver.len(), 1, "the job was still sent");
    }

    /// An answer the world published before it ended is still delivered: the
    /// world owed it and paid, and ending afterwards takes nothing back.
    #[test]
    fn an_answer_published_before_the_world_ended_is_delivered() {
        let (mut pending, jobs, receiver) = asking(WorldState::Stopped, Some(100.0));
        let mut world = World::new();
        pending
            .request(&jobs, 1, |_: &mut World| 7)
            .expect("the queue has room");
        drain(&receiver, &mut world);

        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::Answered(7))]);
    }

    #[test]
    fn a_late_answer_does_not_settle_a_retry_under_the_same_key() {
        let (mut pending, jobs, receiver) = asking(WorldState::Running, Some(1.0));
        let mut world = World::new();
        pending.request(&jobs, 1, |_| 7).unwrap();
        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::Expired)]);
        pending.request(&jobs, 1, |_| 8).unwrap();
        receiver.try_recv().unwrap()(&mut world);
        assert!(settled(&mut pending, 0.0).is_empty());
        receiver.try_recv().unwrap()(&mut world);
        assert_eq!(settled(&mut pending, 0.0), vec![(1, Settled::Answered(8))]);
    }

    /// An answer that arrived for a request already given up on is dropped
    /// rather than delivered late.
    #[test]
    fn an_answer_for_a_request_given_up_on_is_dropped() {
        let (mut pending, jobs, receiver) = asking(WorldState::Running, Some(1.0));
        let mut world = World::new();
        pending
            .request(&jobs, 1, |_: &mut World| 7)
            .expect("the queue has room");

        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::Expired)]);

        // The job runs on a drain after the wait is over, which is what a
        // world that was a tick behind looks like from here.
        drain(&receiver, &mut world);
        assert!(
            settled(&mut pending, 1.0).is_empty(),
            "the late answer was dropped"
        );
    }

    /// A request refused by the world's queue was never asked: it waits for
    /// nothing and expires to nobody.
    #[test]
    fn a_refused_request_waits_for_nothing() {
        let (mut pending, jobs, receiver) = asking(WorldState::Running, Some(1.0));
        drop(receiver);
        assert_eq!(
            pending
                .request(&jobs, 1, |_: &mut World| 0)
                .expect_err("a stopped world refuses the request")
                .kind(),
            SendErrorKind::Closed
        );

        assert!(
            settled(&mut pending, 1.0).is_empty(),
            "nothing was ever asked"
        );
    }

    /// Asking again under a key already outstanding does not lengthen its
    /// wait: the tick the first was asked on is the one that counts.
    #[test]
    fn asking_again_keeps_the_first_wait() {
        let (mut pending, jobs, _receiver) = asking(WorldState::Running, Some(2.0));
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");
        settled(&mut pending, 1.0);
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");

        // One tick stood between the first ask and this one, so the wait the
        // first ask was given runs out here however recently it was repeated.
        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::Expired)]);
    }

    /// A second request under a key already outstanding says it was
    /// coalesced instead of looking like one that went to the world, and the
    /// work it named never runs.
    #[test]
    fn a_coalesced_request_says_so_and_runs_nothing() {
        let (mut pending, jobs, receiver) = asking(WorldState::Running, None);
        let mut world = World::new();
        assert_eq!(
            pending
                .request(&jobs, 1, |_: &mut World| 7)
                .expect("the queue has room"),
            Asked::Queued
        );
        assert_eq!(
            pending
                .request(&jobs, 1, |_: &mut World| panic!("the coalesced work ran"))
                .expect("a coalesced request is not a failure"),
            Asked::Coalesced
        );

        assert_eq!(
            receiver.len(),
            1,
            "the coalesced request queued no second job"
        );
        drain(&receiver, &mut world);
        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::Answered(7))]);
    }

    /// Coalescing leaves the first request's wait alone: the tick it was
    /// asked on is still the one that counts.
    #[test]
    fn a_coalesced_request_does_not_renew_the_first_wait() {
        let (mut pending, jobs, _receiver) = asking(WorldState::Running, Some(2.0));
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");
        assert!(settled(&mut pending, 1.0).is_empty(), "one tick of two");

        assert_eq!(
            pending
                .request(&jobs, 1, |_: &mut World| 0)
                .expect("a coalesced request is not a failure"),
            Asked::Coalesced
        );
        assert_eq!(
            settled(&mut pending, 1.0),
            vec![(1, Settled::Expired)],
            "the coalesced ask did not restart the clock"
        );
    }

    /// A request the queue refused comes back with the job it never ran, so
    /// asking again on a later tick asks the work the first one meant to.
    #[test]
    fn a_refused_request_hands_its_work_back() {
        let (mut pending, jobs, receiver) = asking(WorldState::Running, None);
        // A queue with one place, taken, so the next request meets a full
        // queue rather than a stopped one.
        let (sender, crowded) = async_channel::bounded(1);
        let full = JobSender::new(sender);
        full.send(Box::new(|_: &mut World| {}))
            .expect("the one place was free");
        let refused = pending
            .request(&full, 1, |_: &mut World| 7)
            .expect_err("a full queue refuses the request");
        assert_eq!(refused.kind(), SendErrorKind::Full);
        assert!(
            settled(&mut pending, 1.0).is_empty(),
            "a request the queue never took waits for nothing"
        );

        // The job came back unrun, so running it now does the work the ask
        // meant to do — though the request that asked for it is over, so the
        // answer lands on nobody.
        let mut world = World::new();
        (refused.into_job())(&mut world);
        assert!(
            settled(&mut pending, 1.0).is_empty(),
            "an answer to a request that was never made settles nothing"
        );
        drop(crowded);

        // The same work, asked again now that the queue has room, answers.
        assert_eq!(
            pending
                .request(&jobs, 1, |_: &mut World| 7)
                .expect("the queue has room"),
            Asked::Queued
        );
        drain(&receiver, &mut world);
        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::Answered(7))]);
    }

    /// A request with no timeout waits as long as the world owes it.
    #[test]
    fn a_request_with_no_timeout_waits_until_the_world_ends() {
        let (mut pending, jobs, _receiver) = asking(WorldState::Running, None);
        pending
            .request(&jobs, 1, |_: &mut World| 0)
            .expect("the queue has room");

        for _ in 0..10 {
            assert!(settled(&mut pending, 1.0).is_empty(), "the wait has no end");
        }
    }
}
