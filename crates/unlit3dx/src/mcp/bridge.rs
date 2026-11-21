//! The MCP transports, wired straight into a world: stdio, and streamable
//! HTTP on a TCP port.
//!
//! Serving the protocol blocks and runs on a thread of its own; the
//! commands it receives are posted through a channel and drained by an
//! event-loop behaviour, so a world — which is not sendable — is only
//! ever touched on its own thread.

use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(feature = "mcp-http")]
use std::net::SocketAddr;

use async_channel::{Receiver, unbounded};
use unlit_ecs::mcp::{McpCommand, WorldEntry, WorldRef};
use unlit_ecs::sync;
use unlit_ecs::{Bundle, Command, DynamicBundle, Entity, EntityAccessError, World, bundle};
use unlit3d::winit::event::OnAboutToWait;

use crate::world::{WorldName, WorldSender, drain_all_stepped_worlds};

/// Knocks the host awake when a transport posts a command.
///
/// A host with a window is woken by its event loop, which runs a turn often
/// enough that waiting on it costs nothing. A headless host has no such turn
/// of its own: it waits here instead of polling, and a command is what ends
/// the wait.
#[derive(Clone)]
pub(crate) struct InboxWake(Arc<(Mutex<bool>, std::sync::Condvar)>);

impl InboxWake {
    /// Wake the host waiting on this handle.
    pub(crate) fn wake(&self) {
        *sync::lock(&self.0.0) = true;
        self.0.1.notify_all();
    }

    /// Wait for a command to be posted, or for `timeout` to pass.
    ///
    /// A spurious wake returns early rather than waiting out the timeout: the
    /// caller re-reads the world anyway, and a turn that finds nothing is the
    /// same turn it would have taken at the timeout.
    pub(crate) fn wait(&self, timeout: Duration) {
        let mut woke = sync::lock(&self.0.0);
        if !*woke {
            let (guard, _) = self
                .0
                .1
                .wait_timeout(woke, timeout)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            woke = guard;
        }
        *woke = false;
    }
}

/// The wake every transport serving one host rings.
///
/// One per host rather than one per transport: a host wakes on whichever of
/// its transports posted, so a second transport's command does not wait out
/// the poll timeout behind a wake nobody rang.
#[derive(Clone)]
struct HostWake(InboxWake);

/// The host's wake, made on first use and shared by every transport after.
fn host_wake(world: &mut World) -> InboxWake {
    let existing = world
        .root_scope()
        .query::<&HostWake>()
        .iter()
        .next()
        .map(|wake| wake.0.clone());
    if let Some(wake) = existing {
        return wake;
    }
    let wake = InboxWake(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
    world
        .root_scope_mut()
        .spawn(bundle![HostWake(wake.clone())]);
    wake
}

/// The handle that wakes a host waiting for its transports, when it has one.
///
/// A host with no transport wired in has nothing to wait on, and its caller
/// falls back to polling.
pub(crate) fn inbox_wake(world: &World) -> Option<InboxWake> {
    world
        .root_scope()
        .query::<&McpInbox>()
        .iter()
        .next()
        .and_then(|_| {
            world
                .root_scope()
                .query::<&HostWake>()
                .iter()
                .next()
                .map(|wake| wake.0.clone())
        })
}

/// The commands the transport thread has posted for the host to run.
///
/// Spawned with the behaviour that drains it: an inbox nothing drains fills up
/// and is never read, and a drain behaviour without one has nothing to read.
#[derive(Clone)]
struct McpInbox {
    receiver: Arc<Receiver<McpCommand>>,
}

impl Bundle for McpInbox {
    fn into_bundle(self) -> DynamicBundle {
        bundle![self, drain_mcp()]
    }
}

/// Serve the MCP stdio transport against `world`.
///
/// The transport thread lives as long as its peer stays connected; the
/// inbox behaviour drains whatever arrives every turn of the event loop.
pub fn serve_mcp(world: &mut World) {
    let (sender, receiver) = unbounded::<McpCommand>();
    let wake = host_wake(world);
    let ring = wake.clone();
    let started = std::thread::Builder::new()
        .name("unlit3dx-mcp-transport".to_owned())
        .spawn(move || {
            // An unbounded inbox refuses a command only once its receiver
            // is gone, which is to say once nothing drains the world any
            // more; the pump treats a refused command as the end of the
            // transport, so say why there is no one left to answer.
            let sink = move |command| match sender.try_send(command) {
                Ok(()) => {
                    ring.wake();
                    true
                }
                Err(error) => {
                    log::warn!("the MCP inbox is gone: {error}");
                    false
                }
            };
            if let Err(error) = unlit_ecs::mcp::serve_stdio_with(sink) {
                log::error!("the MCP server stopped: {error}");
            }
        });
    match started {
        Ok(_) => {
            world.root_scope_mut().spawn(McpInbox {
                receiver: Arc::new(receiver),
            });
        }
        Err(error) => log::error!("the MCP transport thread failed to start: {error}"),
    }
}

/// Serve the MCP streamable-HTTP transport against `world` on `addr`.
///
/// The call returns once the port is bound, naming the address it was
/// given — the caller that asked for port 0 learns the port here. The
/// server runs on a thread of its own and lives as long as `world` does:
/// the handle is a component of it, so a world that dies stops serving,
/// and the inbox behaviour drains whatever arrives every turn of the
/// event loop.
///
/// # Errors
///
/// Fails when the port cannot be bound, carrying the underlying message.
#[cfg(feature = "mcp-http")]
pub fn serve_http_mcp(world: &mut World, addr: SocketAddr) -> Result<SocketAddr, String> {
    let (sender, receiver) = unbounded::<McpCommand>();
    let wake = host_wake(world);
    let ring = wake.clone();
    let sink = move |command| match sender.try_send(command) {
        Ok(()) => {
            ring.wake();
            true
        }
        Err(error) => {
            log::warn!("the MCP inbox is gone: {error}");
            false
        }
    };
    let server = unlit_ecs::mcp::serve_http_with(sink, addr).map_err(|error| error.to_string())?;
    let bound = server.local_addr();
    world.root_scope_mut().spawn(McpInbox {
        receiver: Arc::new(receiver),
    });
    // The handle is the component that keeps the server alive: the world
    // dropping it stops the transport.
    world.root_scope_mut().spawn(bundle![server]);
    Ok(bound)
}

/// The event-loop behaviour that runs the commands the transport posted.
///
/// Running a command needs a mutable world, so each one is queued and
/// applied between two event callbacks. The host answers the world list
/// itself: it is the only one that knows which entities carry a world —
/// a nested one, or a sender to one that runs apart from it.
fn drain_mcp() -> OnAboutToWait {
    OnAboutToWait::new(move |world, _entity, ()| drain(world))
}

/// Drain the commands every transport posted.
///
/// Each `serve_mcp` call spawns its own inbox, so a world served twice -- over
/// stdio and, say, a test's stream pair -- has two. Draining one and leaving
/// the other unread would hold that transport's clients waiting for answers
/// that never come.
fn drain(world: &World) {
    let mut mcp_inbox_rows = world.root_scope().query::<&McpInbox>();

    for inbox in mcp_inbox_rows.iter() {
        while let Ok(command) = inbox.receiver.try_recv() {
            dispatch_command(world, command);
        }
    }
}

/// Run one MCP turn without a window event loop.
///
/// A headless host calls this between its own frames. Commands are applied
/// before the next frame, just as they are between two window callbacks. A
/// command for a world the host steps is queued as a job of that world, so the
/// turn ends by draining those queues: the reply must not wait for a frame the
/// client has not asked for yet.
///
/// Returns the names of the stepped worlds whose queued jobs panicked during
/// the turn — see [`drain_all_stepped_worlds`].
pub fn pump_mcp(world: &mut World) -> Result<Vec<&'static str>, EntityAccessError> {
    drain(world);
    world.apply()?;
    Ok(drain_all_stepped_worlds(world))
}

/// Run one MCP command against the world it names.
///
/// A command for a world the host merely carries is queued as a command of
/// the host's own, because only the host's thread may touch its world. The
/// list names every world a tool can reach: the host's own, each nested
/// world an entity carries, and each world an entity holds a
/// [`WorldSender`] for — a world on a thread of its own or one the host
/// steps, neither of which a nested-world lookup could find.
fn dispatch_command(world: &World, command: McpCommand) {
    if command.wants_worlds() {
        command.answer_worlds(reachable_worlds(world));
        return;
    }
    match command.world().entity() {
        Some(entity) => world
            .root_scope()
            .queue()
            .push(RunInWorld { entity, command }),
        None => world.root_scope().queue().push(RunInHost { command }),
    }
}

/// Every world a tool can name through the host, with its name.
///
/// An entity that carries a world without a [`WorldName`] is left out
/// rather than reported under a made-up name: every world a host offers to
/// the tools is one it can name.
fn reachable_worlds(world: &World) -> Vec<WorldEntry> {
    let mut worlds = vec![WorldEntry {
        world: WorldRef::Host,
        name: "host",
    }];
    let push = |entity: Entity, worlds: &mut Vec<WorldEntry>| {
        let id = WorldRef::handle(entity);
        let Ok(mut name) = world.root_scope().try_query_one::<&WorldName>(entity) else {
            return;
        };
        if worlds.iter().any(|entry| entry.world == id) {
            return;
        }
        worlds.push(WorldEntry {
            world: id,
            name: name.get().0,
        });
    };
    for (_world, entity) in world.root_scope().query::<(&World, Entity)>().iter() {
        push(entity, &mut worlds);
    }
    for (_world_sender, entity) in world.root_scope().query::<(&WorldSender, Entity)>().iter() {
        push(entity, &mut worlds);
    }
    worlds
}

/// Run an MCP command against the host world itself.
struct RunInHost {
    command: McpCommand,
}

impl Command for RunInHost {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        self.command.dispatch(world);
        Ok(())
    }
}

/// Run an MCP command against the world one entity carries.
struct RunInWorld {
    entity: Entity,
    command: McpCommand,
}

impl Command for RunInWorld {
    /// A nested world is a component here, so its commands run on this
    /// thread. A world the host only sends to — one on a thread of its own,
    /// or one the host steps — is reached by posting the command as a job
    /// instead: its queue is shared with the handle its spawn returned, so
    /// the sender beside the proxy is enough. The reply rides the command's
    /// own channel, so nothing here waits for the other world to answer.
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        let RunInWorld { entity, command } = *self;
        if let Ok(mut nested) = world.root_scope().try_query_one::<&mut World>(entity) {
            command.dispatch(&mut *nested.get());
            return Ok(());
        }
        let Ok(sender) = world
            .root_scope()
            .try_query_one::<&WorldSender>(entity)
            .map(|mut sender| {
                let sender = sender.get();
                sender.clone()
            })
        else {
            command.fail(format!("entity {entity:?} carries no world"));
            return Ok(());
        };
        if sender.is_finished() {
            command.fail(format!("the world on {entity:?} has stopped"));
            return Ok(());
        }
        // The command rides inside the job, so a slot keeps it here: a
        // refused job hands it back, and can then be failed with the reason.
        let pending = Arc::new(Mutex::new(Some(command)));
        let job_slot = Arc::clone(&pending);
        let job = Box::new(move |world: &mut World| {
            if let Some(command) = sync::lock(&job_slot).take() {
                command.dispatch(world);
            }
        });
        if let Err(error) = sender.send(job)
            && let Some(command) = sync::lock(&pending).take()
        {
            command.fail(format!(
                "the world on {entity:?} refused the command: {error}"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use unlit_ecs::mcp::Output;

    use super::*;
    use crate::FRAME_SECONDS;
    use crate::thread::ThreadControl;
    use crate::world::{spawn_stepped_world_with_init, spawn_threaded_world, step_stepped_world};

    /// A component the host command counts.
    struct Marker;

    /// A host world wired like `serve_mcp` wires one, plus the channel
    /// the fake transport posts commands through.
    fn host_with_inbox() -> (World, async_channel::Sender<McpCommand>) {
        let (host, sender, _wake) = host_with_wake();
        (host, sender)
    }

    /// The same wiring, with the handle the host waits on.
    fn host_with_wake() -> (World, async_channel::Sender<McpCommand>, InboxWake) {
        let mut host = World::new();
        let (sender, receiver) = unbounded::<McpCommand>();
        let wake = host_wake(&mut host);
        host.root_scope_mut().spawn(McpInbox {
            receiver: Arc::new(receiver),
        });
        (host, sender, wake)
    }

    /// Two transports serving one host ring the same wake, so a command on
    /// either ends the host's wait instead of waiting out the poll timeout.
    #[test]
    fn every_transport_of_one_host_rings_the_same_wake() {
        let mut host = World::new();
        let first = host_wake(&mut host);
        let second = host_wake(&mut host);
        let waiter = std::thread::spawn(move || {
            let start = Instant::now();
            second.wait(Duration::from_secs(30));
            start.elapsed()
        });
        std::thread::sleep(Duration::from_millis(50));
        // The first transport posts, and rings the handle it holds.
        first.wake();
        let waited = waiter.join().expect("the waiter survives");
        assert!(
            waited < Duration::from_secs(5),
            "a post on one transport ends a wait on another: {waited:?}"
        );
    }

    /// A waited turn ends as soon as a transport posts, rather than at its
    /// timeout: the wait is the headless host's whole idle loop.
    #[test]
    fn a_wait_ends_when_the_transport_posts() {
        let (_host, transport, wake) = host_with_wake();
        let waiter = {
            let wake = wake.clone();
            std::thread::spawn(move || {
                let start = Instant::now();
                wake.wait(Duration::from_secs(30));
                start.elapsed()
            })
        };
        // The pump rings the handle once the transport has the command, so
        // this is the same order the real turn runs in.
        std::thread::sleep(Duration::from_millis(50));
        let (command, _reply) = McpCommand::list_worlds();
        transport.try_send(command).expect("the inbox takes it");
        wake.wake();
        let waited = waiter.join().expect("the waiter survives");
        assert!(
            waited < Duration::from_secs(5),
            "the wait ended on the ring, not the timeout: {waited:?}"
        );
    }

    /// A host waiting with nothing posted waits out its timeout instead of
    /// spinning: the handle is a wait, not a poll.
    #[test]
    fn a_wait_with_nothing_posted_lasts_its_timeout() {
        let (_host, _transport, wake) = host_with_wake();
        let start = Instant::now();
        wake.wait(Duration::from_millis(120));
        let waited = start.elapsed();
        assert!(
            waited >= Duration::from_millis(100),
            "the wait lasted its timeout rather than returning at once: {waited:?}"
        );
    }

    /// A post between two turns is not slept through: the flag is what the
    /// next wait reads first.
    #[test]
    fn a_wait_that_was_ringed_returns_at_once() {
        let (_host, _transport, wake) = host_with_wake();
        wake.wake();
        let start = Instant::now();
        wake.wait(Duration::from_secs(30));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "a ringed wait returns rather than sleeping"
        );
        // The flag is cleared by the wait, so the turn that follows sleeps.
        let start = Instant::now();
        wake.wait(Duration::from_millis(120));
        assert!(
            start.elapsed() >= Duration::from_millis(100),
            "the ring is spent once it has woken a turn"
        );
    }

    /// Run one turn of the host's event loop: the drain behaviour takes
    /// whatever the transport posted and queues it, then the queued
    /// commands apply — the order the real loop runs them in.
    fn run_turn(host: &mut World) {
        for on_about_to_wait in host.root_scope().query::<&mut OnAboutToWait>().iter() {
            on_about_to_wait.run(host, Entity::from_bits(0), &());
        }
        host.apply().unwrap();
    }

    /// The host answers a world list with itself and every entity that
    /// carries a world, nested or apart — the render sub-world among them.
    #[test]
    fn list_worlds_answers_with_the_host_and_its_nested_worlds() {
        let (mut host, transport) = host_with_inbox();
        let nested = host
            .root_scope_mut()
            .spawn(bundle![World::new(), WorldName("nested")]);

        let (command, reply) = McpCommand::list_worlds();
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);

        let answer = reply.recv_blocking().expect("the command was answered");
        let Ok(Output::Json(value)) = answer else {
            panic!("a world list answers with JSON")
        };
        let text = value.to_string();
        assert!(text.contains("host"), "the host names itself: {text}");
        assert!(
            text.contains("nested") && text.contains(&nested.to_bits().to_string()),
            "the host names its nested world {nested:?}: {text}"
        );
    }

    /// A world the host cannot name is left out of the list: every world the
    /// tools are offered is one the host can name, so nothing is reported
    /// under a made-up label.
    #[test]
    fn list_worlds_leaves_out_a_world_it_cannot_name() {
        let (mut host, transport) = host_with_inbox();
        let unnamed = host.root_scope_mut().spawn(bundle![World::new()]);

        let (command, reply) = McpCommand::list_worlds();
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);

        let answer = reply.recv_blocking().expect("the command was answered");
        let Ok(Output::Json(value)) = answer else {
            panic!("a world list answers with JSON")
        };
        let text = value.to_string();
        assert!(
            !text.contains(&unnamed.to_bits().to_string()),
            "the unnamed world is left out: {text}"
        );
    }

    /// A command aimed at a host entity that carries no world fails with
    /// a readable message instead of being dropped or panicking.
    #[test]
    fn a_command_for_a_worldless_entity_fails_readably() {
        let (mut host, transport) = host_with_inbox();
        let bare = host.root_scope_mut().spawn(bundle![Marker]);

        let (command, reply) = McpCommand::new(|_| Ok(Output::Json(serde_json::Value::Null)));
        let command = command.with_world(WorldRef::handle(bare));
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);

        let answer = reply.recv_blocking().expect("the command was answered");
        let Err(message) = answer else {
            panic!("the command fails")
        };
        assert!(
            message.contains("carries no world"),
            "the failure says why: {message}"
        );
    }

    /// A command aimed at a nested world runs against that world, not the
    /// host: the marker only the nested world carries is the one counted.
    #[test]
    fn a_command_runs_against_the_nested_world_it_names() {
        let (mut host, transport) = host_with_inbox();
        let mut nested = World::new();
        nested.root_scope_mut().spawn(bundle![Marker]);
        let nested_world = host.root_scope_mut().spawn(bundle![nested]);

        let (command, reply) = McpCommand::new(|world| {
            let count = world.root_scope().query::<&Marker>().iter().count();
            Ok(Output::Json(serde_json::Value::from(count as u64)))
        });
        let command = command.with_world(WorldRef::handle(nested_world));
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);

        let answer = reply.recv_blocking().expect("the command was answered");
        let Ok(Output::Json(value)) = answer else {
            panic!("the job answers with JSON")
        };
        assert_eq!(value.as_u64(), Some(1), "the nested world was seen");
    }

    /// A command aimed at the host runs against the host world itself.
    #[test]
    fn a_host_command_runs_against_the_host_world() {
        let (mut host, transport) = host_with_inbox();
        host.root_scope_mut().spawn(bundle![Marker]);

        let (command, reply) = McpCommand::new(|world| {
            let count = world.root_scope().query::<&Marker>().iter().count();
            Ok(Output::Json(serde_json::Value::from(count as u64)))
        });
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);

        let answer = reply.recv_blocking().expect("the command was answered");
        let Ok(Output::Json(value)) = answer else {
            panic!("the job answers with JSON")
        };
        assert_eq!(value.as_u64(), Some(1), "the host world was seen");
    }
    /// A world the host merely sends to is named too, so a tool can reach a
    /// world that runs apart from the host — an example's gameplay world, say.
    #[test]
    fn list_worlds_names_a_world_on_its_own_thread() {
        let (mut host, transport) = host_with_inbox();
        let threaded = spawn_threaded_world(&mut host, "listed", 8, |_, control, _| {
            control.wait_timeout(Duration::from_millis(1));
            true
        });

        let (command, reply) = McpCommand::list_worlds();
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);

        let answer = reply.recv_blocking().expect("the command was answered");
        let Ok(Output::Json(value)) = answer else {
            panic!("a world list answers with JSON")
        };
        let text = value.to_string();
        assert!(
            text.contains("listed") && text.contains(&threaded.entity.to_bits().to_string()),
            "the host names the threaded world: {text}"
        );
    }

    /// A command aimed at a world on its own thread runs there and answers
    /// on the command's own channel: the host never waits for the thread.
    #[test]
    fn a_command_runs_against_a_world_on_its_own_thread() {
        let (mut host, transport) = host_with_inbox();
        let threaded = spawn_threaded_world(&mut host, "counted", 8, |_, control, _| {
            control.wait_timeout(Duration::from_millis(1));
            true
        });
        let seeded = crate::channel::Snapshot::new();
        let reader = seeded.reader();
        let publish = seeded.clone();
        threaded
            .sender
            .send(Box::new(move |world: &mut World| {
                world.root_scope_mut().spawn(bundle![Marker]);
                publish.publish(());
            }))
            .expect("the thread accepts jobs");
        let deadline = Instant::now() + Duration::from_secs(2);
        while reader.latest().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(reader.latest().is_some(), "the thread ran the seeding job");

        let (command, reply) = McpCommand::new(|world| {
            let count = world.root_scope().query::<&Marker>().iter().count();
            Ok(Output::Json(serde_json::Value::from(count as u64)))
        });
        let command = command.with_world(WorldRef::handle(threaded.entity));
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);

        let answer = reply.recv_blocking().expect("the command was answered");
        let Ok(Output::Json(value)) = answer else {
            panic!("the job answers with JSON")
        };
        assert_eq!(value.as_u64(), Some(1), "the threaded world was seen");
    }

    /// A stepped world is named and reached the same way, and its command
    /// runs on a step: nothing else drives it.
    #[test]
    fn a_command_runs_against_a_world_the_host_steps() {
        let (mut host, transport) = host_with_inbox();
        let stepped = spawn_stepped_world_with_init(
            &mut host,
            "stepped",
            8,
            |world: &mut World| {
                world.root_scope_mut().spawn(bundle![Marker]);
            },
            |_, _: &ThreadControl, (), _: f32| true,
        );

        let (command, reply) = McpCommand::new(|world| {
            let count = world.root_scope().query::<&Marker>().iter().count();
            Ok(Output::Json(serde_json::Value::from(count as u64)))
        });
        let command = command.with_world(WorldRef::handle(stepped.entity));
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);
        step_stepped_world(&host, stepped.entity, FRAME_SECONDS).expect("the stepped world starts");

        let answer = reply.recv_blocking().expect("the command was answered");
        let Ok(Output::Json(value)) = answer else {
            panic!("the job answers with JSON")
        };
        assert_eq!(value.as_u64(), Some(1), "the stepped world was seen");
    }

    /// A world that has stopped answers nothing, so the host explains why
    /// instead of leaving the job to be dropped without a word.
    #[test]
    fn a_command_for_a_stopped_world_fails_readably() {
        let (mut host, transport) = host_with_inbox();
        let stepped = spawn_stepped_world_with_init(
            &mut host,
            "ended",
            8,
            |_: &mut World| (),
            |_, _: &ThreadControl, (), _: f32| false,
        );
        step_stepped_world(&host, stepped.entity, FRAME_SECONDS).expect("the stepped world starts");
        assert!(stepped.is_finished(), "the tick ended the world");

        let (command, reply) = McpCommand::new(|_| Ok(Output::Json(serde_json::Value::Null)));
        let command = command.with_world(WorldRef::handle(stepped.entity));
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);

        let answer = reply.recv_blocking().expect("the command was answered");
        let Err(message) = answer else {
            panic!("the command fails")
        };
        assert!(
            message.contains("has stopped"),
            "the failure says why: {message}"
        );
    }

    /// A command whose job the queue refuses fails right here with the
    /// reason, rather than closing the channel without a word.
    #[test]
    fn a_command_for_a_full_queue_fails_readably() {
        let (mut host, transport) = host_with_inbox();
        let stepped = spawn_stepped_world_with_init(
            &mut host,
            "busy",
            1,
            |_: &mut World| (),
            |_, _: &ThreadControl, (), _: f32| true,
        );
        stepped
            .sender
            .send(Box::new(|_: &mut World| {}))
            .expect("the first job fits");

        let (command, reply) = McpCommand::new(|_| Ok(Output::Json(serde_json::Value::Null)));
        let command = command.with_world(WorldRef::handle(stepped.entity));
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);

        let answer = reply.recv_blocking().expect("the command was answered");
        let Err(message) = answer else {
            panic!("the command fails")
        };
        assert!(
            message.contains("refused the command"),
            "the failure says why: {message}"
        );
    }
}
