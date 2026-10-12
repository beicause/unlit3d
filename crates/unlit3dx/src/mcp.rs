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
