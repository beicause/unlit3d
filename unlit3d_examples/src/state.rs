use super::*;

/// Events delivered to the winit loop from outside a `WindowEvent`.
pub(crate) enum UserEvent {
    /// The async GPU setup finished; the scene is built from it here, on the
    /// main thread.
    Ready(Gpu),
    /// The async GPU setup failed; the message is reported and the app exits.
    Failed(String),
    /// A command from the MCP transport, to run against a world of the host.
    ///
    /// It arrives on this thread because a world is not `Send`; the app
    /// resolves the command's world handle and dispatches it here, between
    /// frames, so no command ever runs while a frame is being drawn.
    ///
    /// The command is wrapped in an `Option` because a user event reaches its
    /// callbacks mutably and the one that runs it takes it out.
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    Command(Option<unlit_ecs::mcp::McpCommand>),
}

/// The GPU context a scene draws with, requested asynchronously.
///
/// The type is [`unlit_wgpu::context::GpuContext`]'s, shared with the other
/// entry points in this repository. Everything in it is a `Send + Sync` wgpu
/// handle, so the request can run off the event loop's thread and the context
/// can cross back to it. The scene itself cannot cross: its ECS world is
/// single-threaded, so it is built on the main thread once this arrives.
///
/// The context and its device outlive a suspension: rebuilding them would drop
/// every pipeline, buffer and texture the scene holds, for a change the
/// platform only makes to the native window.
use unlit_wgpu::context::GpuContext as Gpu;

/// Where the one-time GPU setup stands.
///
/// The request is asynchronous, so a resume that arrives before it lands must
/// not start a second one — two devices and two adapters would be built, and
/// only one of them could present. The `Requested` state is what remembers
/// that the setup is already in flight.
#[derive(Default)]
pub(crate) enum GpuState {
    /// No request has been made: the loop is before its first resume, or the
    /// first one failed and the app is exiting.
    #[default]
    Idle,
    /// The request is in flight; `user_event` carries its result.
    Requested,
    /// The context is here and the scene can draw with it.
    Ready(Gpu),
}

/// Set by the windowed shell's scene-selector panel, read once by the frame
/// loop.
pub(crate) struct SceneSwitch(pub(crate) Option<&'static scenes::SceneDef>);

/// Set by the windowed shell's fullscreen button, read once by the frame loop.
///
/// A flag rather than the state to move to: the button asks for the window to
/// change, and which way it changes is read from the window itself at the
/// moment the request is served. A panel runs while the world is borrowed to
/// lay the UI out, so it can only record the click; the frame loop owns the
/// window the request is served on. See [`fullscreen`].
pub(crate) struct FullscreenRequest(pub(crate) bool);

/// Whether the window is fullscreen, written once per frame by the frame loop.
///
/// The button labels itself for the action it would take, and a panel has no
/// window to ask, so the frame loop reads the window's state and leaves it here
/// for the panel to draw from.
pub(crate) struct FullscreenActive(pub(crate) bool);

/// The host entity carrying the scene's world and its [`Scene`].
///
/// `None` until the GPU context arrives. The world itself outlives a
/// suspension — only the swap chain it is presented through does not — so the
/// scene's state survives one.
pub(crate) struct SceneEntity(pub(crate) Option<Entity>);

/// The render world an application presents through, under the name it reports
/// to the MCP tools.
///
/// A world is reachable from the host only as a component of an entity, and the
/// name is what the report lists it by: spawning the two together is what makes
/// the scene findable at all.
pub(crate) struct RenderWorld {
    /// The world the scene draws in.
    pub(crate) world: World,
    /// The scene driving it.
    pub(crate) scene: Scene,
    /// The name the MCP tools list it under.
    pub(crate) name: &'static str,
}

impl Bundle for RenderWorld {
    fn name(&self) -> &'static str {
        self.name
    }

    fn into_bundle(self) -> DynamicBundle {
        bundle![self.world, self.scene, WorldName(self.name)]
    }
}

/// The name this example reports a host world under to the MCP tools.
///
/// The example keeps its own component rather than borrowing the core's: it
/// does not depend on unlit3dx, and a name is all the report needs. Targets
/// without the MCP dependency never read it back.
#[cfg_attr(
    any(target_arch = "wasm32", target_os = "android"),
    expect(
        dead_code,
        reason = "only the MCP bridge reads the name back, and it is not built for these targets"
    )
)]
pub(crate) struct WorldName(pub(crate) &'static str);

/// The entity carrying the presentation path, absent until the scene and
/// surface are ready.
///
/// The path is its own entity rather than a component of the app because the
/// library's frame behaviours find it by querying for a [Presenter]; dropping
/// the entity is how a scene switch gives the path up.
pub(crate) struct PresenterEntity(pub(crate) Option<Entity>);

/// The scene the example starts with; a switch replaces it.
pub(crate) struct InitialScene(pub(crate) &'static scenes::SceneDef);

/// The selector window's rectangle the last time a scene was live, so a scene
/// switch can rebuild it in the same place and at the same size.
pub(crate) struct SelectorPos(pub(crate) Option<egui::Rect>);

/// The GPU-info panel's rectangle the last time a scene was live, so a scene
/// switch can rebuild it where the user left it.
pub(crate) struct GpuInfoPos(pub(crate) Option<egui::Rect>);

/// The application's state, spawned as one object.
///
/// Every component is its own cell, so a callback writing a sibling state
/// component never conflicts with the behaviour being run. The behaviours are
/// spawned beside this entity rather than in it, each handed the handle back, so
/// a callback reaches the state it reads and the proxy it posts its own events
/// through from one place.
pub(crate) struct App {
    /// The scene the application starts with.
    pub(crate) scene: &'static scenes::SceneDef,
    /// The proxy a callback posts an event to the loop it runs on with.
    pub(crate) proxy: EventLoopProxy<UserEvent>,
    /// Whether to serve the world over the Model Context Protocol.
    pub(crate) mcp: bool,
}

impl Bundle for App {
    fn into_bundle(self) -> DynamicBundle {
        bundle![
            GpuState::Idle,
            SceneEntity(None),
            PresenterEntity(None),
            InitialScene(self.scene),
            SelectorPos(None),
            GpuInfoPos(None),
            McpEnabled(self.mcp),
            ExitRequest(false),
            WinitProxy(self.proxy),
            FrameSkip::default(),
        ]
    }
}

/// Where the scene's draggable panels opened, carried from the scene a switch
/// replaced.
///
/// The two rectangles travel together because they are one idea: each is the
/// position egui remembered for a panel, read from the dying scene's context
/// and handed to the new scene's. A scene built without a carried position
/// opens its panels in their default corners.
#[derive(Clone, Copy, Default)]
pub struct PanelRects {
    /// The scene-selector window's last rectangle.
    pub selector: Option<egui::Rect>,
    /// The GPU-info panel's last rectangle.
    pub gpu_info: Option<egui::Rect>,
}

impl PanelRects {
    /// The panels' rectangles as the app entity remembers them.
    pub(crate) fn read(world: &World, app: Entity) -> Self {
        Self {
            selector: world
                .root_scope()
                .query_one::<&SelectorPos>(app)
                .ok()
                .and_then(|mut pos| pos.get().0),
            gpu_info: world
                .root_scope()
                .query_one::<&GpuInfoPos>(app)
                .ok()
                .and_then(|mut pos| pos.get().0),
        }
    }
}

/// Whether to serve the world over the Model Context Protocol rather than
/// drive a scene on this thread.
///
/// False on the web and on Android, which have no stdio transport; see
/// [`MCP_SUPPORTED`].
pub(crate) struct McpEnabled(pub(crate) bool);

/// The interval a scene's index advances at, in seconds, or `None` to advance
/// it once per drawn frame.
pub(crate) struct SequenceStep(pub(crate) Option<f32>);

/// Seconds accumulated towards the next advance of a scene's index.
pub(crate) struct SequenceClock(pub(crate) f32);

/// Whether this frame advanced the scene's index, decided by the sequence step
/// and read by the behaviour that runs the scene's own advance.
pub(crate) struct SequenceStepped(pub(crate) bool);

/// A scene's own per-frame behaviour, stored in its world so the frame
/// behaviour that drives it can call it with the world it mutates.
///
/// `None` only while it is being called: the closure needs `&mut World`, and a
/// component cannot be borrowed mutably and handed the world it lives in at the
/// same time, so it is taken out, called, and put back.
pub(crate) struct SceneAdvance(pub(crate) Option<Advance>);

/// The scene's per-frame state, spawned with its own advance.
///
/// The state and the behaviour that moves it live on one entity so a frame
/// advances the scene without `&mut Scene`: the index the sequence step keeps
/// and the clock it accumulates into are read by the advance beside them, and
/// neither half means anything without the other.
pub(crate) struct SceneFrame {
    /// The scene's own per-frame behaviour.
    pub(crate) advance: Advance,
    /// The interval the scene's index advances at, or `None` to advance it once
    /// per drawn frame.
    pub(crate) step: Option<f32>,
}
