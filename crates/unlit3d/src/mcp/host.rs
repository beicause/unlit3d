//! The rendering tools: the resource graph, buffers, textures, input, meshes
//! and frames.
//!
//! A tool names the entity handles it needs and this module resolves them
//! through the public API, reporting a readable error when a handle does not
//! name what the tool asked for. The tools themselves live one level up, in
//! [crate::mcp].

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::Engine as _;

use unlit_ecs::mcp::ToolError;
use unlit_ecs::mcp::host::to_value;
use unlit_ecs::mcp::serde_json::{self, Value};
use unlit_ecs::{Entity, Scope, World};
use unlit_wgpu::resources::{ResId, ResourceGraph, ResourceGraphInfo, ResourceGraphMaintain};

use crate::gltf::LitGltf;
use crate::prelude::*;
use crate::reflect;

mod readback;

pub(crate) use self::readback::{
    read_buffer, read_screenshot, read_texture_as_image, save_buffer, save_screenshot,
    save_texture_as_image,
};

/// The render context the world holds.
///
/// The context is a singleton, spawned by [`spawn_context`]
/// when the world was given a device; a world without one has no graph to read.
fn render_context(world: &World) -> Result<RenderContext, ToolError> {
    RenderContext::of(world).ok_or_else(|| ToolError::rejected("the world has no render context"))
}

/// The entity carrying the world's renderer, from `handle` or by query.
fn renderer_entity(world: &World, handle: Option<Entity>) -> Result<Entity, ToolError> {
    if let Some(entity) = handle {
        return world
            .root_scope()
            .try_query_one::<Has<Renderer>>(entity)
            .is_ok_and(|mut holder| holder.get())
            .then_some(entity)
            .ok_or_else(|| ToolError::rejected(format!("entity {entity:?} carries no renderer")));
    }
    let mut found: Option<Entity> = None;
    for (_renderer, entity) in world.root_scope().query::<(&Renderer, Entity)>().iter() {
        if found.is_some() {
            return Err(ToolError::rejected(
                "the world has more than one renderer; pass `renderer`",
            ));
        }
        found = Some(entity);
    }
    found.ok_or_else(|| ToolError::rejected("the world has no renderer"))
}

/// The entity carrying the world's mesh source, from `handle` or by query.
fn mesh_source_entity(world: &World, handle: Option<Entity>) -> Result<Entity, ToolError> {
    if let Some(entity) = handle {
        let is_mesh_source = world
            .root_scope()
            .try_query_one::<&Source>(entity)
            .is_ok_and(|mut source| source.get().as_ref::<MeshSource>().is_ok());
        return is_mesh_source.then_some(entity).ok_or_else(|| {
            ToolError::rejected(format!("entity {entity:?} carries no mesh source"))
        });
    }
    let mut found: Option<Entity> = None;
    for (source, entity) in world.root_scope().query::<(&Source, Entity)>().iter() {
        if source.as_ref::<MeshSource>().is_ok() {
            if found.is_some() {
                return Err(ToolError::rejected(
                    "the world has more than one mesh source; pass `source`",
                ));
            }
            found = Some(entity);
        }
    }
    found.ok_or_else(|| ToolError::rejected("the world has no mesh source"))
}

/// Run `f` with the mesh source `handle` names, or the world's only one.
fn with_mesh_source<R>(
    world: &World,
    handle: Option<Entity>,
    f: impl FnOnce(&mut MeshSource, &Scope<'_>) -> R,
) -> Result<R, ToolError> {
    let entity = mesh_source_entity(world, handle)?;
    // The scope is the source's own: a source entity is its own scope, and a
    // source only ever sees the subtree it draws.
    let scope = world
        .try_scope(entity)
        .map_err(|_| ToolError::rejected("the mesh source entity is gone"))?;
    let mut source = scope
        .try_query_one::<&mut Source>(entity)
        .map_err(|_| ToolError::rejected("the mesh source entity is gone"))?;
    let mesh_source = source
        .get()
        .as_mut::<MeshSource>()
        .map_err(|error| ToolError::failed(error.to_string()))?;
    Ok(f(mesh_source, &scope))
}

/// The device the context names, cloned out of the world.
fn graph_of<'w>(
    world: &'w World,
    context: RenderContext,
) -> Result<unlit_ecs::QueryOne<'w, &'w ResourceGraph>, ToolError> {
    world
        .root_scope()
        .try_query_one::<&ResourceGraph>(context.graph)
        .map_err(|_| ToolError::rejected("the render context has no resource graph"))
}

/// The graph the context names, mutably borrowed from the world.
fn graph_of_mut<'w>(
    world: &'w World,
    context: RenderContext,
) -> Result<unlit_ecs::QueryOne<'w, &'w mut ResourceGraph>, ToolError> {
    world
        .root_scope()
        .try_query_one::<&mut ResourceGraph>(context.graph)
        .map_err(|_| ToolError::rejected("the render context has no resource graph"))
}

/// Whether input events were delivered.
#[derive(facet::Facet)]
struct Delivered {
    /// Whether the world had events to dispatch. This is the batch's own size,
    /// not a count of the handlers that ran: an event nothing watches still
    /// counts, because the world did receive it.
    delivered: bool,
}

/// The entities a glTF scene spawned.
#[derive(facet::Facet)]
struct SpawnedEntities {
    /// The spawned entities, as bare bits.
    #[facet(opaque, proxy = unlit_ecs::EntityVecProxy)]
    entities: Vec<Entity>,
    /// The names of those entities, in the same order.
    names: Vec<&'static str>,
}

pub(crate) fn graph_summary(world: &World) -> Result<Value, ToolError> {
    let context = render_context(world)?;
    let mut graph_row = graph_of(world, context)?;
    let graph = graph_row.get();
    let info: ResourceGraphInfo = graph.info();
    to_value(&info)
}

/// Every resource slot in the graph.
pub(crate) fn list_resources(world: &World) -> Result<Value, ToolError> {
    let context = render_context(world)?;
    let mut graph_row = graph_of(world, context)?;
    let graph = graph_row.get();
    to_value(&graph.resource_infos())
}

/// The resources a resource depends on, as handles.
pub(crate) fn resource_dependencies(world: &World, id: ResId) -> Result<Value, ToolError> {
    let context = render_context(world)?;
    let mut graph_row = graph_of(world, context)?;
    let graph = graph_row.get();
    let handle = graph
        .resolve(id)
        .ok_or_else(|| ToolError::rejected(format!("stale resource id {}", id.to_bits())))?;
    let ids = graph
        .dependencies(&handle)
        .map(|dependency| dependency.id())
        .collect::<Vec<_>>();
    to_value(&ids)
}

/// Drop unreferenced resources and rebuild dirty ones.
pub(crate) fn graph_maintain(world: &World) -> Result<Value, ToolError> {
    let context = render_context(world)?;
    let mut graph_row = graph_of_mut(world, context)?;
    let graph = graph_row.get();
    let before = graph.len();
    graph.maintain();
    let after = graph.len();
    drop(graph_row);
    to_value(&ResourceGraphMaintain { before, after })
}

/// The entity carrying `T` that `handle` names, or the world's only one.
fn single_entity<T: 'static>(
    world: &World,
    handle: Option<Entity>,
    what: &str,
) -> Result<Entity, ToolError> {
    if let Some(entity) = handle {
        return world
            .root_scope()
            .try_query_one::<Has<T>>(entity)
            .is_ok_and(|mut holder| holder.get())
            .then_some(entity)
            .ok_or_else(|| ToolError::rejected(format!("entity {entity:?} carries no {what}")));
    }
    let mut found: Option<Entity> = None;
    for (_t, entity) in world.root_scope().query::<(&T, Entity)>().iter() {
        if found.is_some() {
            return Err(ToolError::rejected(format!(
                "the world has more than one {what}; pass `entity`"
            )));
        }
        found = Some(entity);
    }
    found.ok_or_else(|| ToolError::rejected(format!("the world has no {what}")))
}

/// A world's input state, encoded: how its devices are.
pub(crate) fn input_state(world: &World, entity: Option<Entity>) -> Result<Value, ToolError> {
    let entity = single_entity::<InputState>(world, entity, "input state")?;
    let text = reflect::encode_json(&world.root_scope(), entity, "InputState")
        .map_err(|error| ToolError::failed(error.to_string()))?;
    serde_json::from_str(&text).map_err(|error| ToolError::failed(error.to_string()))
}

/// Feed input events to the world and deliver them.
///
/// The events go to the world's [`InputWriter`], which reaches every world
/// subscribed to it; the dispatch that follows is this world's own tick.
pub(crate) fn send_input(
    world: &World,
    entity: Option<Entity>,
    events: &Value,
) -> Result<Value, ToolError> {
    let text =
        serde_json::to_string(events).map_err(|error| ToolError::failed(error.to_string()))?;
    let decoded =
        reflect::decode_events(&text).map_err(|error| ToolError::failed(error.to_string()))?;
    let entity = single_entity::<InputWriter>(world, entity, "input writer")?;
    world
        .root_scope()
        .try_query_one::<&mut InputWriter>(entity)
        .map(|mut writer| {
            let writer = writer.get();
            writer.push_all(decoded)
        })
        .map_err(|error| {
            ToolError::failed(format!(
                "entity {entity:?} carries no input writer: {error}"
            ))
        })?;
    let delivered = dispatch_input(world);
    to_value(&Delivered { delivered })
}

/// Render one frame from the world.
pub(crate) fn render_frame(
    world: &mut World,
    renderer: Option<Entity>,
) -> Result<Value, ToolError> {
    let entity = renderer_entity(world, renderer)?;
    let world_ref: &World = &*world;
    world_ref
        .root_scope()
        .try_query_one::<&mut Renderer>(entity)
        .map(|mut renderer| renderer.get().render(world_ref))
        .map_err(|error| {
            ToolError::failed(format!("entity {entity:?} carries no renderer: {error}"))
        })?;
    Ok(Value::Null)
}

/// Spawn the default scene of a glTF document read from a file or inline.
///
/// Exactly one of `path` and `document` must be given. A document is either
/// glTF JSON text or a GLB whose bytes are base64-encoded, since a JSON tool
/// argument cannot carry raw binary.
pub(crate) fn spawn_gltf(
    world: &mut World,
    source: Option<Entity>,
    scope: Option<Entity>,
    path: Option<&str>,
    document: Option<&str>,
) -> Result<Value, ToolError> {
    let gltf = match (path, document) {
        (Some(path), None) => LitGltf::load(Path::new(path)),
        (None, Some(document)) => LitGltf::from_bytes(&gltf_document_bytes(document)?),
        (Some(_), Some(_)) => {
            return Err(ToolError::rejected(
                "pass either `path` or `document`, not both",
            ));
        }
        (None, None) => return Err(ToolError::rejected("pass one of `path` and `document`")),
    }
    .map_err(gltf_failure)?;
    // Content goes into the requested scope, which defaults to the source
    // entity itself, so the frame that draws the source also draws the scene.
    let scope = match scope {
        Some(scope) => scope,
        None => mesh_source_entity(world, source)?,
    };
    // Asked before anything uploads: a scope that names nothing is answered
    // with its own error rather than with a document's worth of resources the
    // caller never gets a handle to.
    world
        .try_scope(scope)
        .map_err(|_| ToolError::rejected(format!("no scope {scope:?}")))?;
    let asset = with_mesh_source(world, source, |source, scope| {
        gltf.insert_asset(source, scope)
    })?
    .map_err(gltf_failure)?;
    let root = world
        .try_scope_mut(scope)
        .map_err(|_| ToolError::rejected(format!("no scope {scope:?}")))?
        .spawn_named("lit model", LitModelBundle::new(asset.model));
    let mut entities = vec![root];
    let mut cursor = 0;
    while cursor < entities.len() {
        let parent = entities[cursor];
        let children = world.children(parent).map_err(|error| {
            ToolError::failed(format!("cannot enumerate model child {parent:?}: {error}"))
        })?;
        entities.extend(children);
        cursor += 1;
    }
    let names = entities.iter().map(|entity| entity.name()).collect();
    to_value(&SpawnedEntities { entities, names })
}

/// A glTF failure, with its own structure kept beside the message.
///
/// The message is what a person reads; the `gltf` detail says which check
/// failed and which mesh, primitive or image to correct, so a client repairs
/// the document instead of matching on prose.
fn gltf_failure(error: crate::gltf::Error) -> ToolError {
    let details = serde_json::json!({ "gltf": error.to_value() });
    ToolError::failed(error.to_string()).with_details(details)
}

/// The bytes of an inline glTF document: JSON text as written, or a GLB's
/// bytes decoded from base64.
fn gltf_document_bytes(document: &str) -> Result<Vec<u8>, ToolError> {
    // A JSON document is the one form a caller can paste; anything else is
    // taken as base64, so the leading brace decides without guessing at the
    // alphabet. Some editors write a byte-order mark, which is legal UTF-8 but
    // would hide the brace, so it is stripped before the decision — decoding
    // the document would then fail on a byte that carries no data.
    let document = document.strip_prefix('\u{feff}').unwrap_or(document);
    if document.trim_start().starts_with('{') {
        return Ok(document.as_bytes().to_vec());
    }
    base64::engine::general_purpose::STANDARD
        .decode(document.trim())
        .map_err(|error| ToolError::failed(format!("the document is not base64: {error}")))
}

/// Shared frame statistics a monitor behavior feeds and the frame-clock tool
/// reads.
#[derive(Default)]
struct FrameMonitor {
    /// Durations of the most recent frames, front = oldest.
    window: VecDeque<f32>,
}

/// How many recent frame durations the rolling average covers.
const FRAME_WINDOW: usize = 120;

impl FrameMonitor {
    /// Fold one frame's duration into the statistics.
    fn record(&mut self, delta_time: f32) {
        self.window.push_back(delta_time);
        if self.window.len() > FRAME_WINDOW {
            self.window.pop_front();
        }
    }

    /// Mean duration over the rolling window, if any frame was recorded.
    fn average_delta(&self) -> Option<f32> {
        let total: f32 = self.window.iter().sum();
        (!self.window.is_empty()).then(|| total / self.window.len() as f32)
    }
}

/// The statistics the monitor behavior shares with the frame-clock tool.
struct FrameMonitorState {
    monitor: Arc<Mutex<FrameMonitor>>,
}

/// The monitor behavior spawned on the first frame-clock query: it feeds the
/// shared statistics with each frame the world's driver dispatches.
fn frame_monitor(monitor: Arc<Mutex<FrameMonitor>>) -> OnFrame {
    OnFrame::new(move |_world, _entity, frame| {
        // A frame dispatched with a zero duration would drive the average to
        // an infinite fps, so it is left out of the window rather than
        // folded in. The clock still counts it, being what the driver states.
        // A panic elsewhere poisons the lock but not the window it guards: a
        // queue of durations has no invariant a half-finished update breaks, so
        // the monitor keeps the mean rather than losing the whole statistic.
        if frame.delta_time.as_secs() > 0.0 {
            monitor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .record(frame.delta_time.as_secs_f32());
        }
    })
}

/// What the frame-clock tools report about one world.
///
/// Every field but the averages is the world's own [`FrameClock`], so a world
/// that never dispatched a frame reports none of them: a client can tell a
/// world with no pacing of its own from one standing at its first frame.
#[derive(Debug, Clone, Copy, PartialEq, facet::Facet)]
pub(crate) struct FrameClockReport {
    /// The world's frame ordinal, or none when it never dispatched a frame.
    index: Option<u32>,
    /// How long the world's last frame took, in seconds.
    delta_time: Option<f32>,
    /// The time the world's frames have covered, in seconds.
    time: Option<f32>,
    /// The rate the last frame ran at, in frames per second.
    fps: Option<f32>,
    /// The average rate over the measured window, or none before the monitor
    /// has seen a frame.
    average_fps: Option<f32>,
    /// How many frames the average covers.
    average_window_frames: Option<usize>,
}

/// Report a world's frame ordinal, last frame duration, elapsed time and
/// frame rate, spawning the monitor that measures the average on first call.
///
/// The frame state is the world's own [`FrameClock`], which a world that
/// dispatches frames spawns when it initialises. A world that never dispatched
/// a frame — the host world of a windowed run, which carries no scene — reports
/// no frame rather than a made-up zero, so a client can tell the two apart.
/// The average covers the last [`FRAME_WINDOW`] frames, which is finer than a
/// client can sample for itself and survives the frames between two calls.
pub(crate) fn frame_clock(world: &mut World) -> Result<FrameClockReport, ToolError> {
    let existing = world
        .root_scope()
        .query::<&FrameMonitorState>()
        .iter()
        .next()
        .map(|state| state.monitor.clone());
    let monitor = match existing {
        Some(monitor) => monitor,
        None => {
            let monitor = Arc::new(Mutex::new(FrameMonitor::default()));
            let state = FrameMonitorState {
                monitor: monitor.clone(),
            };
            let behavior = frame_monitor(monitor.clone());
            world.root_scope_mut().spawn(bundle![behavior, state]);
            monitor
        }
    };
    let monitor = monitor
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let clock = world
        .root_scope()
        .query::<&FrameClock>()
        .iter()
        .next()
        .copied();
    // A zero-duration frame would report an infinite rate; leave the
    // instantaneous fps out instead.
    let fps = clock.and_then(|clock| {
        (clock.delta_time.as_secs() > 0.0).then(|| 1.0 / clock.delta_time.as_secs_f32())
    });
    let average_fps = monitor
        .average_delta()
        .filter(|delta| *delta > 0.0)
        .map(|delta| 1.0 / delta);
    let window_frames = monitor.window.len();
    Ok(FrameClockReport {
        index: clock.map(|clock| clock.index),
        delta_time: clock.map(|clock| clock.delta_time.as_secs_f32()),
        time: clock.map(|clock| clock.time),
        fps,
        average_fps,
        average_window_frames: average_fps.map(|_| window_frames),
    })
}

#[cfg(test)]
mod tests;
