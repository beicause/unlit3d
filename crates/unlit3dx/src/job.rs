//! Jobs: closures that run against another world.

use crossbeam_channel::{Receiver, bounded};
use unlit_ecs::World;

/// A unit of work handed to another world: runs once against it, on the
/// thread that world lives on.
pub type Job<R = ()> = Box<dyn FnOnce(&mut World) -> R + Send + 'static>;

/// A job whose result comes back to the sending thread.
///
/// Sending a request never blocks on the work itself: the job runs whenever
/// the remote world reaches it, and the result travels back through a
/// one-slot channel the request owns. Poll [`poll`](Self::poll) from a
/// frame loop, or block on [`recv`](Self::recv) when nothing else remains
/// to do.
pub struct Request<R> {
    /// The one-slot channel the wrapped job sends the result through.
    reply: Receiver<R>,
}

impl<R: Send + 'static> Request<R> {
    /// Wrap a job so its result returns to the sender, yielding the job to
    /// send and the handle to poll.
    ///
    /// A dropped handle means nobody is waiting any more; the result dies
    /// with it, which the wrapper treats as success.
    pub fn wrap(job: Job<R>) -> (Job<()>, Self) {
        let (reply, receiver) = bounded(1);
        let job = Box::new(move |world: &mut World| {
            let _ = reply.send(job(world));
        });
        (job, Self { reply: receiver })
    }

    /// The result, if it has landed.
    #[must_use]
    pub fn poll(&self) -> Option<R> {
        self.reply.try_recv().ok()
    }

    /// The result, blocking until it lands.
    ///
    /// Panics when the remote world stopped before running the job.
    pub fn recv(self) -> R {
        self.reply.recv().expect("the remote world ran the request")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wrapped job sends its result through the handle.
    #[test]
    fn wrap_sends_the_result_through_the_handle() {
        let (job, request) = Request::wrap(Box::new(|_: &mut World| 2 + 2));
        let mut world = World::new();
        job(&mut world);
        assert_eq!(request.poll(), Some(4));
    }

    /// Polling before the job runs finds nothing, and blocking waits.
    #[test]
    fn poll_is_empty_before_the_job_runs() {
        let (_job, request) = Request::wrap(Box::new(|_: &mut World| 1));
        assert_eq!(request.poll(), None);
    }
}
