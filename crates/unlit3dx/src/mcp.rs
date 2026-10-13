//! The MCP stdio transport, wired straight into a world.
//!
//! Serving the protocol blocks and runs on a thread of its own; the
//! commands it receives are posted through a channel and drained by an
//! event-loop behaviour, so a world — which is not sendable — is only
//! ever touched on its own thread.

use std::sync::Arc;

use async_channel::{Receiver, unbounded};
use unlit_ecs::{Command, Entity, World};
use unlit3d::winit::event::OnAboutToWait;
use unlit3d_mcp::{McpCommand, WorldId};

/// The commands the transport thread has posted for the host to run.
#[derive(Clone)]
struct McpInbox {
    receiver: Arc<Receiver<McpCommand>>,
}

/// Serve the MCP stdio transport against `world`.
///
/// The transport thread lives as long as its peer stays connected; the
/// inbox behaviour drains whatever arrives every turn of the event loop.
pub fn serve_mcp(world: &mut World) {
    let (sender, receiver) = unbounded::<McpCommand>();
    let started = std::thread::Builder::new()
        .name("unlit3dx-mcp-transport".to_owned())
        .spawn(move || {
            let sink = move |command| sender.try_send(command).is_ok();
            if let Err(error) = unlit3d_mcp::serve_stdio_with(sink) {
                log::error!("the MCP server stopped: {error}");
            }
        });
    match started {
        Ok(_) => {
            world.spawn((
                McpInbox {
                    receiver: Arc::new(receiver),
                },
                drain_mcp(),
            ));
        }
        Err(error) => log::error!("the MCP transport thread failed to start: {error}"),
    }
}

/// The event-loop behaviour that runs the commands the transport posted.
///
/// Running a command needs a mutable world, so each one is queued and
/// applied between two event callbacks. The host answers the world list
/// itself: it is the only one that knows which entities carry a world.
fn drain_mcp() -> OnAboutToWait {
    OnAboutToWait::new(move |world, _entity, ()| {
        let Some(inbox) = world.query::<&McpInbox>().next().map(|(_, inbox)| inbox) else {
            return;
        };
        while let Ok(command) = inbox.receiver.try_recv() {
            dispatch_command(world, command);
        }
    })
}

/// Run one MCP command against the world it names.
fn dispatch_command(world: &World, command: McpCommand) {
    if command.wants_worlds() {
        let mut worlds = vec![WorldId::Host];
        worlds.extend(
            world
                .query::<(Entity, &World)>()
                .map(|(entity, _)| WorldId::handle(entity)),
        );
        command.answer_worlds(worlds);
        return;
    }
    match command.world().entity() {
        Some(entity) => world.queue().push(RunInWorld { entity, command }),
        None => world.queue().push(RunInHost { command }),
    }
}

/// Run an MCP command against the host world itself.
struct RunInHost {
    command: McpCommand,
}

impl Command for RunInHost {
    fn apply(self: Box<Self>, world: &mut World) {
        self.command.dispatch(world);
    }
}

/// Run an MCP command against the nested world one entity carries.
struct RunInWorld {
    entity: Entity,
    command: McpCommand,
}

impl Command for RunInWorld {
    fn apply(self: Box<Self>, world: &mut World) {
        if let Some(mut nested) = world.get_mut::<World>(self.entity) {
            self.command.dispatch(&mut nested);
        } else {
            self.command
                .fail(format!("entity {} carries no world", self.entity.to_bits()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlit3d_mcp::Output;

    /// A component the host command counts.
    struct Marker;

    /// A host world wired like `serve_mcp` wires one, plus the channel
    /// the fake transport posts commands through.
    fn host_with_inbox() -> (World, async_channel::Sender<McpCommand>) {
        let mut host = World::new();
        let (sender, receiver) = unbounded::<McpCommand>();
        host.spawn((
            McpInbox {
                receiver: Arc::new(receiver),
            },
            drain_mcp(),
        ));
        (host, sender)
    }

    /// Run one turn of the host's event loop: the drain behaviour takes
    /// whatever the transport posted and queues it, then the queued
    /// commands apply — the order the real loop runs them in.
    fn run_turn(host: &mut World) {
        for (_, mut behaviour) in host.query::<&mut OnAboutToWait>() {
            behaviour.run(host, Entity::from_bits(0), &());
        }
        host.apply();
    }

    /// The host answers a world list with itself and every entity that
    /// carries a nested world — the render sub-world among them.
    #[test]
    fn list_worlds_answers_with_the_host_and_its_nested_worlds() {
        let (mut host, transport) = host_with_inbox();
        let nested = host.spawn((World::new(),));

        let (command, reply) = McpCommand::list_worlds();
        transport.try_send(command).expect("the inbox takes it");
        run_turn(&mut host);

        let answer = reply.recv_blocking().expect("the command was answered");
        let Ok(Output::Json(value)) = answer else {
            panic!("a world list answers with JSON")
        };
        let text = value.to_string();
        assert!(text.contains("Host"), "the host names itself: {text}");
        assert!(
            text.contains(&nested.to_bits().to_string()),
            "the host names its nested world {nested:?}: {text}"
        );
    }

    /// A command aimed at a host entity that carries no world fails with
    /// a readable message instead of being dropped or panicking.
    #[test]
    fn a_command_for_a_worldless_entity_fails_readably() {
        let (mut host, transport) = host_with_inbox();
        let bare = host.spawn((Marker,));

        let (command, reply) = McpCommand::new(|_| Ok(Output::Json(serde_json::Value::Null)));
        let command = command.with_world(WorldId::handle(bare));
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
        nested.spawn((Marker,));
        let nested_world = host.spawn((nested,));

        let (command, reply) = McpCommand::new(|world| {
            let count = world.query::<&Marker>().count();
            Ok(Output::Json(serde_json::Value::from(count as u64)))
        });
        let command = command.with_world(WorldId::handle(nested_world));
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
        host.spawn((Marker,));

        let (command, reply) = McpCommand::new(|world| {
            let count = world.query::<&Marker>().count();
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
}
