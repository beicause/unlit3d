//! Send a job and poll for its answer without blocking the calling world.

use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_channel::{Receiver, bounded};
use hashbrown::HashMap;
use unlit_ecs::World;
use web_time::Instant;

use crate::endpoint::WorldEndpoint;
use crate::job::SendError;
use crate::thread::WorldState;
use crate::world::WorldError;

use crate::time::WallTimeout;

/// Whether asking queued a job or reused the outstanding request for its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Asked {
    /// A job was queued.
    Queued,
    /// The supplied job was not queued and did not renew the existing wait.
    Coalesced,
}

/// How one request ended.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Settled<T> {
    /// The job returned a value, which may itself express an operation error.
    Answered(T),
    /// The caller stopped waiting, without interrupting the job.
    TimedOut,
    /// The world stopped before answering.
    WorldStopped,
    /// The world failed before answering.
    WorldFailed(WorldError),
    /// The job observed and acknowledged a cancellation request.
    Cancelled,
}

/// The cancellation signal for one job. A checkpoint does not undo earlier work.
#[derive(Clone, Debug)]
pub struct CancellationToken(Arc<AtomicBool>);

/// A cancellation acknowledged by the job through a token checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the job acknowledged cancellation")]
pub struct Cancelled(());

impl CancellationToken {
    /// Return an acknowledgment once the caller has requested cancellation.
    /// Propagate the error to end the job; ignoring it does not cancel anything.
    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        if self.0.load(Ordering::Acquire) {
            Err(Cancelled(()))
        } else {
            Ok(())
        }
    }
}

struct Outstanding<T> {
    answer: Receiver<Result<T, Cancelled>>,
    queued_at: Instant,
    cancellation: CancellationToken,
}

/// The requests one caller is waiting for from a single world endpoint.
/// A dropped or timed-out request stops waiting without cancelling its job.
pub struct Pending<K, T> {
    world: WorldEndpoint,
    timeout: Option<WallTimeout>,
    outstanding: HashMap<K, Outstanding<T>>,
}

impl<K, T> Pending<K, T>
where
    K: Eq + Hash + Clone,
    T: Send + 'static,
{
    /// Construct a request set. `None` waits until the world answers or ends.
    #[must_use]
    pub fn new(world: WorldEndpoint, timeout: Option<WallTimeout>) -> Self {
        Self {
            world,
            timeout,
            outstanding: HashMap::new(),
        }
    }

    /// Queue a job. Cancellation can prevent it from starting, but cannot
    /// interrupt this closure after it starts. Use [`Self::request_cancellable`]
    /// to add checkpoints while the job is running.
    pub fn request<F>(&mut self, key: K, make: F) -> Result<Asked, SendError>
    where
        F: FnOnce(&mut World) -> T + Send + 'static,
    {
        self.request_cancellable(key, move |world, _| Ok(make(world)))
    }

    /// Queue a job that may acknowledge cancellation at its checkpoints.
    ///
    /// The world checks the token before invoking `make`. An outstanding key
    /// coalesces without replacing the job, its token or its deadline. A send
    /// refusal returns the job, but no answer to it will settle any request.
    pub fn request_cancellable<F>(&mut self, key: K, make: F) -> Result<Asked, SendError>
    where
        F: FnOnce(&mut World, &CancellationToken) -> Result<T, Cancelled> + Send + 'static,
    {
        if self.outstanding.contains_key(&key) {
            return Ok(Asked::Coalesced);
        }
        let cancellation = CancellationToken(Arc::new(AtomicBool::new(false)));
        let token = cancellation.clone();
        let (send, answer) = bounded(1);
        let queued_at = Instant::now();
        self.world.commands().send(Box::new(move |world| {
            let result = token.checkpoint().and_then(|()| make(world, &token));
            let _ = send.try_send(result);
        }))?;
        self.outstanding.insert(
            key,
            Outstanding {
                answer,
                queued_at,
                cancellation,
            },
        );
        Ok(Asked::Queued)
    }

    /// Request cancellation for an outstanding key without waiting for it.
    /// `true` means the signal was sent, not that the job acknowledged it.
    pub fn cancel(&self, key: &K) -> bool {
        let Some(request) = self.outstanding.get(key) else {
            return false;
        };
        request.cancellation.0.store(true, Ordering::Release);
        true
    }

    /// Replace `settled` with results observed on this poll, each reported once.
    /// The results have no guaranteed order.
    /// Answers and acknowledged cancellation take precedence over world exit;
    /// world exit takes precedence over timeout. Polling does not drive the world.
    pub fn poll(&mut self, settled: &mut Vec<(K, Settled<T>)>) {
        self.poll_at(Instant::now(), settled);
    }

    fn poll_at(&mut self, now: Instant, settled: &mut Vec<(K, Settled<T>)>) {
        settled.clear();
        // Read the status first, so an answer published before the terminal
        // status cannot disappear between a failed receive and the status read.
        let state = self.world.state().latest();
        let timeout = self.timeout;
        self.outstanding.retain(|key, request| {
            let outcome = match request.answer.try_recv() {
                Ok(Ok(value)) => Settled::Answered(value),
                Ok(Err(Cancelled(()))) => Settled::Cancelled,
                Err(_) => match state.as_deref() {
                    Some(WorldState::Stopped) => Settled::WorldStopped,
                    Some(WorldState::Failed(error)) => Settled::WorldFailed(error.clone()),
                    _ if timeout.is_some_and(|WallTimeout(wait)| {
                        now.saturating_duration_since(request.queued_at) >= wait
                    }) =>
                    {
                        Settled::TimedOut
                    }
                    _ => return true,
                },
            };
            settled.push((key.clone(), outcome));
            false
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use unlit_ecs::Entity;

    use crate::endpoint::{WorldDriving, WorldOwnership};
    use crate::job::{Job, JobSender, SendErrorKind};
    use crate::thread::ThreadStatus;

    /// The endpoint of a world that has already reached `state`, reached
    /// through `commands`.
    fn endpoint_for(state: WorldState, commands: JobSender) -> WorldEndpoint {
        let status = ThreadStatus::detached();
        match state {
            WorldState::Failed(error) => status.record_failure(error),
            WorldState::Stopped => status.finish(),
            WorldState::Running => {}
        }
        WorldEndpoint::new(
            Entity::from_bits(1),
            commands,
            status,
            WorldOwnership::HostOwned,
            WorldDriving::Stepped,
        )
    }

    /// Requests for a world in `state`, and the queue its jobs land in.
    fn asking(
        state: WorldState,
        timeout: Option<f32>,
    ) -> (Pending<u32, u32>, async_channel::Receiver<Job>) {
        let (sender, receiver) = async_channel::unbounded();
        let endpoint = endpoint_for(state, JobSender::new(sender));
        (
            Pending::new(
                endpoint,
                timeout.map(|seconds| WallTimeout(Duration::from_secs_f32(seconds))),
            ),
            receiver,
        )
    }

    /// Run every job queued so far.
    fn drain(receiver: &async_channel::Receiver<Job>, world: &mut World) {
        while let Ok(job) = receiver.try_recv() {
            job(world);
        }
    }

    fn settled(
        pending: &mut Pending<u32, u32>,
        seconds_since_ask: f32,
    ) -> Vec<(u32, Settled<u32>)> {
        let mut settled = Vec::new();
        let now = pending
            .outstanding
            .values()
            .map(|request| request.queued_at)
            .max()
            .unwrap_or_else(Instant::now)
            + Duration::from_secs_f32(seconds_since_ask);
        pending.poll_at(now, &mut settled);
        settled
    }

    /// An answer reaches the caller under the key it was asked with, however
    /// many were asked at once.
    #[test]
    fn an_answer_comes_back_under_its_key() {
        let (mut pending, receiver) = asking(WorldState::Running, None);
        let mut world = World::new();
        for key in 0..4u32 {
            pending
                .request(key, move |_: &mut World| key * 10)
                .expect("the queue has room");
        }
        drain(&receiver, &mut world);

        let mut answers = settled(&mut pending, 1.0);
        answers.sort_unstable_by_key(|(key, _)| *key);
        assert_eq!(
            answers,
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

    #[test]
    fn cancellation_is_not_reported_until_the_job_checks_the_token() {
        let (mut pending, receiver) = asking(WorldState::Running, None);
        pending
            .request(1, |_| panic!("the cancelled body must not start"))
            .unwrap();
        assert!(pending.cancel(&1));
        assert!(settled(&mut pending, 0.0).is_empty());
        drain(&receiver, &mut World::new());
        assert_eq!(settled(&mut pending, 0.0), vec![(1, Settled::Cancelled)]);
        assert!(!pending.cancel(&1));
    }

    #[test]
    fn a_running_job_can_acknowledge_cancellation_at_a_checkpoint() {
        let (mut pending, receiver) = asking(WorldState::Running, None);
        let (started, start) = std::sync::mpsc::channel();
        let (resume, wait) = std::sync::mpsc::channel();
        pending
            .request_cancellable(1, move |_, token| {
                started.send(()).unwrap();
                wait.recv().unwrap();
                token.checkpoint()?;
                Ok(7)
            })
            .unwrap();
        let job = receiver.try_recv().unwrap();
        let worker = std::thread::spawn(move || job(&mut World::new()));
        start.recv().unwrap();
        assert!(pending.cancel(&1));
        resume.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(settled(&mut pending, 0.0), vec![(1, Settled::Cancelled)]);
    }

    #[test]
    fn a_refused_job_cannot_answer_a_later_request_under_the_same_key() {
        let (send, queue) = bounded(1);
        send.try_send(Box::new(|_: &mut World| {}) as Job).unwrap();
        let endpoint = endpoint_for(WorldState::Running, JobSender::new(send));
        let mut pending = Pending::new(endpoint, None);
        let refused = pending.request(1, |_| 7).unwrap_err();
        let _ = queue.try_recv().unwrap();
        pending.request(1, |_| 8).unwrap();
        let mut world = World::new();
        refused.into_job()(&mut world);
        assert!(settled(&mut pending, 0.0).is_empty());
        drain(&queue, &mut world);
        assert_eq!(settled(&mut pending, 0.0), vec![(1, Settled::Answered(8))]);
    }

    #[test]
    fn a_failed_world_settles_the_request_with_its_cause() {
        let error = WorldError::Panic {
            stage: crate::world::WorldStage::Job,
            message: "failed".into(),
        };
        let (mut pending, _receiver) = asking(WorldState::Failed(error.clone()), None);
        pending.request(1, |_| 7).unwrap();
        assert_eq!(
            settled(&mut pending, 0.0),
            vec![(1, Settled::WorldFailed(error))]
        );
    }

    /// A request whose job never ran is given up on once its time runs out,
    /// and not before.
    #[test]
    fn a_request_is_given_up_on_after_its_time_runs_out() {
        let (mut pending, _receiver) = asking(WorldState::Running, Some(2.0));
        pending
            .request(1, |_: &mut World| 0)
            .expect("the queue has room");

        assert!(
            settled(&mut pending, 1.0).is_empty(),
            "one tick of two is spent"
        );
        assert_eq!(settled(&mut pending, 2.0), vec![(1, Settled::TimedOut)]);
    }

    /// A world that ended owes nothing: every request still outstanding is
    /// given up on at once, without waiting for its time to run out.
    #[test]
    fn an_ended_world_answers_nothing() {
        let (mut pending, receiver) = asking(WorldState::Stopped, Some(100.0));
        pending
            .request(1, |_: &mut World| 0)
            .expect("the queue has room");

        assert_eq!(
            settled(&mut pending, 1.0),
            vec![(1, Settled::WorldStopped)],
            "the job sat in the queue of a world that never ran it"
        );
        assert_eq!(receiver.len(), 1, "the job was still sent");
    }

    /// An answer the world published before it ended is still delivered: the
    /// world owed it and paid, and ending afterwards takes nothing back.
    #[test]
    fn an_answer_published_before_the_world_ended_is_delivered() {
        let (mut pending, receiver) = asking(WorldState::Stopped, Some(100.0));
        let mut world = World::new();
        pending
            .request(1, |_: &mut World| 7)
            .expect("the queue has room");
        drain(&receiver, &mut world);

        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::Answered(7))]);
    }

    #[test]
    fn a_late_answer_does_not_settle_a_retry_under_the_same_key() {
        let (mut pending, receiver) = asking(WorldState::Running, Some(1.0));
        let mut world = World::new();
        pending.request(1, |_| 7).unwrap();
        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::TimedOut)]);
        pending.request(1, |_| 8).unwrap();
        receiver.try_recv().unwrap()(&mut world);
        assert!(settled(&mut pending, 0.0).is_empty());
        receiver.try_recv().unwrap()(&mut world);
        assert_eq!(settled(&mut pending, 0.0), vec![(1, Settled::Answered(8))]);
    }

    /// An answer that arrived for a request already given up on is dropped
    /// rather than delivered late.
    #[test]
    fn an_answer_for_a_request_given_up_on_is_dropped() {
        let (mut pending, receiver) = asking(WorldState::Running, Some(1.0));
        let mut world = World::new();
        pending
            .request(1, |_: &mut World| 7)
            .expect("the queue has room");

        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::TimedOut)]);

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
        let (mut pending, receiver) = asking(WorldState::Running, Some(1.0));
        drop(receiver);
        assert_eq!(
            pending
                .request(1, |_: &mut World| 0)
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
        let (mut pending, _receiver) = asking(WorldState::Running, Some(2.0));
        pending
            .request(1, |_: &mut World| 0)
            .expect("the queue has room");
        settled(&mut pending, 1.0);
        pending
            .request(1, |_: &mut World| 0)
            .expect("the queue has room");

        // One tick stood between the first ask and this one, so the wait the
        // first ask was given runs out here however recently it was repeated.
        assert_eq!(settled(&mut pending, 2.0), vec![(1, Settled::TimedOut)]);
    }

    /// A second request under a key already outstanding says it was
    /// coalesced instead of looking like one that went to the world, and the
    /// work it named never runs.
    #[test]
    fn a_coalesced_request_says_so_and_runs_nothing() {
        let (mut pending, receiver) = asking(WorldState::Running, None);
        let mut world = World::new();
        assert_eq!(
            pending
                .request(1, |_: &mut World| 7)
                .expect("the queue has room"),
            Asked::Queued
        );
        assert_eq!(
            pending
                .request(1, |_: &mut World| panic!("the coalesced work ran"))
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
        let (mut pending, _receiver) = asking(WorldState::Running, Some(2.0));
        pending
            .request(1, |_: &mut World| 0)
            .expect("the queue has room");
        assert!(settled(&mut pending, 1.0).is_empty(), "one tick of two");

        assert_eq!(
            pending
                .request(1, |_: &mut World| 0)
                .expect("a coalesced request is not a failure"),
            Asked::Coalesced
        );
        assert_eq!(
            settled(&mut pending, 2.0),
            vec![(1, Settled::TimedOut)],
            "the coalesced ask did not restart the clock"
        );
    }

    /// A request the queue refused comes back with the job it never ran, so
    /// asking again on a later tick asks the work the first one meant to.
    #[test]
    fn a_refused_request_hands_its_work_back() {
        // A queue with one place, taken, so the next request meets a full
        // queue rather than a stopped one.
        let (sender, crowded) = async_channel::bounded(1);
        let full = endpoint_for(WorldState::Running, JobSender::new(sender));
        full.commands()
            .send(Box::new(|_: &mut World| {}))
            .expect("the one place was free");
        let mut crowded_pending = Pending::new(full, None);
        let refused = crowded_pending
            .request(1, |_: &mut World| 7)
            .expect_err("a full queue refuses the request");
        assert_eq!(refused.kind(), SendErrorKind::Full);
        assert!(
            settled(&mut crowded_pending, 1.0).is_empty(),
            "a request the queue never took waits for nothing"
        );

        // The job came back unrun, so running it now does the work the ask
        // meant to do — though the request that asked for it is over, so the
        // answer lands on nobody.
        let mut world = World::new();
        (refused.into_job())(&mut world);
        assert!(
            settled(&mut crowded_pending, 1.0).is_empty(),
            "an answer to a request that was never made settles nothing"
        );
        drop(crowded);

        // The same work, asked again now that the queue has room, answers.
        let (mut pending, receiver) = asking(WorldState::Running, None);
        assert_eq!(
            pending
                .request(1, |_: &mut World| 7)
                .expect("the queue has room"),
            Asked::Queued
        );
        drain(&receiver, &mut world);
        assert_eq!(settled(&mut pending, 1.0), vec![(1, Settled::Answered(7))]);
    }

    /// A request with no timeout waits as long as the world owes it.
    #[test]
    fn a_request_with_no_timeout_waits_until_the_world_ends() {
        let (mut pending, _receiver) = asking(WorldState::Running, None);
        pending
            .request(1, |_: &mut World| 0)
            .expect("the queue has room");

        for _ in 0..10 {
            assert!(settled(&mut pending, 1.0).is_empty(), "the wait has no end");
        }
    }
}
