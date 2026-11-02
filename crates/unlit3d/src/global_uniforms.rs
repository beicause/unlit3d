//! The frame's shared uniform records, and the buffers they are uploaded into.
//!
//! A draw binds three uniform records at the global group: the camera it is
//! drawn through, the globals the frame advances once, and the view its pass
//! covers. All three belong to the frame rather than to a source. Two sources
//! drawn through one camera read the same bytes, and a source built once per
//! pass reads different ones.
//!
//! A source that uploads them itself writes the same bytes once per build, and
//! with more than one pass it writes them twice before either pass records:
//! every build stages into the frame's one encoder, so the last write wins and
//! the first pass draws with the second pass's camera.
//!
//! [`GlobalUniforms`](crate::global_uniforms::GlobalUniforms) keeps them instead,
//! one buffer per distinct record:
//!
//! * the frame globals, one for the whole frame, advanced by the renderer
//!   before any source builds;
//! * a camera per [`CameraKey`](crate::global_uniforms::CameraKey) -- the entity
//!   a camera hangs off, or the screen mapping a source synthesises for itself;
//! * a view per [`ViewKey`](crate::global_uniforms::ViewKey) -- the viewport a pass draws through.
//!
//! A slot keeps its buffer for as long as the slot lives, so a bind group built
//! from one stays valid from frame to frame, and a slot whose bytes did not
//! change copies nothing. A camera shared by two passes then costs one upload
//! rather than two, and a still camera costs none.
//!
//! The registry lives in the world's shared subtree beside the device and the
//! resource graph, so every source reaches it through the
//! [RenderContext](crate::source::RenderContext).

use core::hash::Hash;

use hashbrown::HashMap;
use unlit_ecs::Entity;
use unlit_wgpu::globals::{Camera, Globals, View};
use unlit_wgpu::resources::{ResHandle, ResId, ResourceGraph};
use unlit_wgpu::staging::StagingBuffer;
use zerocopy::{Immutable, IntoBytes};

/// What a camera record is derived from.
///
/// Two draws naming the same key read the same record: two passes through one
/// entity's camera, or two frames of a screen mapping that has not changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CameraKey {
    /// The camera the entity carries.
    Entity(Entity),
    /// A mapping the source synthesised, which no entity carries: a UI
    /// projects its own points into clip space instead of through a camera
    /// component.
    ///
    /// The key names the viewport the mapping is derived from, so a screen
    /// that was not resized reuses its record.
    Screen {
        /// The viewport width in points, as bits.
        width_bits: u32,
        /// The viewport height in points, as bits.
        height_bits: u32,
    },
}

impl CameraKey {
    /// The key of the screen mapping for a `width` x `height` viewport.
    pub fn screen(width: f32, height: f32) -> Self {
        Self::Screen {
            width_bits: width.to_bits(),
            height_bits: height.to_bits(),
        }
    }
}

/// What a view record is derived from: the viewport a pass draws through.
///
/// Two passes over viewports of one shape share a record even when the
/// viewports sit in different corners of their targets, because only the size
/// reaches the shader: it is the scale a pixel-measuring shader divides by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewKey {
    width_bits: u32,
    height_bits: u32,
}

impl ViewKey {
    /// The key of a `width` x `height` viewport.
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width_bits: width.to_bits(),
            height_bits: height.to_bits(),
        }
    }

    /// The record this key names.
    pub fn record(self) -> View {
        View::new(
            f32::from_bits(self.width_bits),
            f32::from_bits(self.height_bits),
        )
    }
}

/// The identity of the camera and view a bind group was built from.
///
/// A group binds the buffers themselves, so two draws through two cameras need
/// two groups even when they share a pipeline. The frame globals are the same
/// for every draw of the frame and so are not part of the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UniformsKey {
    camera: ResId,
    view: ResId,
}

/// The three records one draw binds at the global group.
///
/// A group built from one of these binds these very buffers, so a draw is
/// rebuilt when the [UniformsKey] it names changes and kept as it is when that
/// key repeats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameUniforms {
    /// The camera record, from the frame or from the source's own mapping.
    pub camera: ResHandle<wgpu::Buffer>,
    /// The frame globals, advanced by the renderer once a frame.
    pub globals: ResHandle<wgpu::Buffer>,
    /// The view record of the viewport the draw lands in.
    pub view: ResHandle<wgpu::Buffer>,
    /// The frame the three records were asked for in, as
    /// [GlobalUniforms::frame] counts it. A source stamps the bind groups it
    /// builds from them with it, so the cache it keeps holds the keys the
    /// frame draws and drops the ones it no longer does.
    pub frame: u64,
}

impl FrameUniforms {
    /// The identity of this camera and view pair.
    pub fn key(&self) -> UniformsKey {
        UniformsKey {
            camera: self.camera.id(),
            view: self.view.id(),
        }
    }

    /// Make `group` depend on the three records it binds.
    ///
    /// A bind group caches the buffers it names, so it has to be rebuilt when
    /// one of them is replaced. The buffers are made once per slot and only
    /// ever rewritten, but a slot is dropped and remade when its camera or its
    /// viewport stops being drawn, so this edge is not merely defensive.
    pub fn declare_dependencies(
        &self,
        graph: &mut ResourceGraph,
        group: &ResHandle<wgpu::BindGroup>,
    ) {
        graph.add_dependency(group, &self.camera);
        graph.add_dependency(group, &self.globals);
        graph.add_dependency(group, &self.view);
    }
}

/// One uniform buffer, the staging pool that fills it, and what it holds.
///
/// The buffer is created once and rewritten in place, never replaced, which is
/// what lets a bind group built from it outlive the frame it was built in.
struct Slot<T> {
    /// The buffer, as the resource graph names it.
    buffer: ResHandle<wgpu::Buffer>,
    /// The buffer itself, for the staging copy to name.
    raw: wgpu::Buffer,
    /// The staging buffers this slot's bytes travel through.
    staging: StagingBuffer,
    /// What the buffer holds. `None` until the first upload, which leaves the
    /// zeroes the device hands out -- the same bytes as the record's
    /// [Default](Default) for all three records.
    uploaded: Option<T>,
    /// Whether the frame being built asked for this slot.
    used: bool,
}

impl<T> Slot<T>
where
    T: Clone + PartialEq + IntoBytes + Immutable,
{
    /// A slot over a newly created, zeroed buffer of the record's size.
    fn new(device: &wgpu::Device, graph: &mut ResourceGraph, label: &str) -> Self {
        let raw = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: core::mem::size_of::<T>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let buffer = graph.insert(raw.clone(), None);
        Self {
            buffer,
            raw,
            staging: StagingBuffer::new(),
            uploaded: None,
            used: false,
        }
    }

    /// Copy `value` into the slot's buffer, unless it already holds it.
    fn upload(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, value: &T) {
        self.used = true;
        if self.uploaded.as_ref() == Some(value) {
            return;
        }
        self.staging
            .write(device, encoder, &self.raw, 0, value.as_bytes());
        self.uploaded = Some(value.clone());
    }
}

/// The buffer a slot named by `key` is, creating the slot on first sight.
fn slot<K, T>(
    slots: &mut HashMap<K, Slot<T>>,
    key: K,
    make: impl FnOnce() -> Slot<T>,
) -> &mut Slot<T>
where
    K: Eq + Hash,
{
    slots.entry(key).or_insert_with(make)
}

/// The frame's camera, globals and view records, one buffer per distinct one.
///
/// Spawn one in the world's shared subtree;
/// [spawn_context](crate::source::spawn_context) does it once per world. The
/// renderer advances the frame globals before it builds any source, and each
/// source asks for the camera and view of the pass it is building.
pub struct GlobalUniforms {
    /// The frame globals, written once per frame.
    globals: Slot<Globals>,
    /// One camera record per distinct camera.
    cameras: HashMap<CameraKey, Slot<Camera>>,
    /// One view record per distinct viewport.
    views: HashMap<ViewKey, Slot<View>>,
    /// How many frames have been begun. A source drops the bind groups it
    /// built for an older one, so the cache it keeps for its cameras and views
    /// is exactly the set the current frame draws.
    frame: u64,
}

impl GlobalUniforms {
    /// A registry holding the frame globals and no camera or view yet.
    ///
    /// # Panics
    ///
    /// If the device cannot hand out a uniform buffer of the globals' size,
    /// which no real device fails to do.
    pub fn new(device: &wgpu::Device, graph: &mut ResourceGraph) -> Self {
        Self {
            globals: Slot::new(device, graph, "unlit3d::globals"),
            cameras: HashMap::new(),
            views: HashMap::new(),
            frame: 0,
        }
    }

    /// The frame being built, as a counter that increases once per frame.
    ///
    /// A source compares it against the one it cached a bind group under: a
    /// group built for an older frame is one nothing draws any more.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Begin a frame: every record becomes unused until a source asks for it.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.globals.used = false;
        for slot in self.cameras.values_mut() {
            slot.used = false;
        }
        for slot in self.views.values_mut() {
            slot.used = false;
        }
    }

    /// Advance the frame globals, the one record every draw of the frame
    /// reads. Called before any source builds.
    pub fn advance_globals(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        globals: &Globals,
    ) {
        self.globals.upload(device, encoder, globals);
    }

    /// The three records for a draw through `camera` in the viewport `view`.
    ///
    /// The camera and view slots are created on first sight and reused
    /// afterwards, so a camera that did not move or a viewport that did not
    /// change costs no copy at all.
    pub fn uniforms(
        &mut self,
        device: &wgpu::Device,
        graph: &mut ResourceGraph,
        encoder: &mut wgpu::CommandEncoder,
        camera: CameraKey,
        camera_record: &Camera,
        view: ViewKey,
    ) -> FrameUniforms {
        self.globals.used = true;
        let camera_slot = slot(&mut self.cameras, camera, || {
            Slot::new(device, graph, "unlit3d::camera")
        });
        camera_slot.upload(device, encoder, camera_record);
        let camera_buffer = camera_slot.buffer.clone();

        let view_slot = slot(&mut self.views, view, || {
            Slot::new(device, graph, "unlit3d::view")
        });
        view_slot.upload(device, encoder, &view.record());
        let view_buffer = view_slot.buffer.clone();

        FrameUniforms {
            camera: camera_buffer,
            globals: self.globals.buffer.clone(),
            view: view_buffer,
            frame: self.frame,
        }
    }

    /// End a frame: drop the camera and view slots nothing asked for.
    ///
    /// A camera entity that is gone takes its record with it, so a world that
    /// spawns a camera per level holds one record rather than one per level.
    /// The buffer is released when the graph is next maintained.
    pub fn end_frame(&mut self) {
        self.cameras.retain(|_, slot| slot.used);
        self.views.retain(|_, slot| slot.used);
    }
}

impl core::fmt::Debug for GlobalUniforms {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GlobalUniforms")
            .field("frame", &self.frame)
            .field("cameras", &self.cameras.len())
            .field("views", &self.views.len())
            .finish()
    }
}

/// One graph node a source built from a frame's uniforms.
struct StampedGroup {
    /// The bind group, as the resource graph names it.
    id: ResHandle<wgpu::BindGroup>,
    /// The frame that last asked for it.
    frame: u64,
}

/// The bind groups a source built from the frame's uniforms, one per
/// [UniformsKey].
///
/// A group binds the buffers themselves, so a pipeline drawn through two
/// cameras or into two viewports needs two groups even though it is one
/// pipeline. A source keeps one of these per pipeline -- or per target, when
/// the pipeline itself is specialized per target -- and asks it for the group
/// of the key the frame draws with.
#[derive(Default)]
pub struct UniformGroups {
    groups: HashMap<UniformsKey, StampedGroup>,
}

impl UniformGroups {
    /// No groups: every key builds one the first time it is asked for.
    pub fn new() -> Self {
        Self::default()
    }

    /// The group for `key`, built by `make` the first time this frame draws
    /// it.
    ///
    /// `make` registers the group in the resource graph and returns the node,
    /// so the group outlives the frame: a key this frame repeats costs a
    /// lookup and nothing else. The caller records `frame`, the frame the
    /// uniforms were asked for in, so that [Self::prune] can tell a group the
    /// frame still draws from one it passed through once.
    pub fn group(
        &mut self,
        graph: &mut ResourceGraph,
        key: UniformsKey,
        frame: u64,
        make: impl FnOnce(&mut ResourceGraph) -> ResHandle<wgpu::BindGroup>,
    ) -> ResHandle<wgpu::BindGroup> {
        if let Some(group) = self.groups.get_mut(&key) {
            group.frame = frame;
            return group.id.clone();
        }
        let id = make(graph);
        self.groups.insert(
            key,
            StampedGroup {
                id: id.clone(),
                frame,
            },
        );
        id
    }

    /// Drop the groups no frame asked for since the one before `frame`.
    ///
    /// A key survives one frame without being drawn, which is what keeps a
    /// source built once per pass from rebuilding its groups every frame: the
    /// passes alternate their keys, and each one's group is still there when
    /// it comes round again. A key that stays away longer is one no pass draws
    /// any more — a camera entity that is gone, or a viewport no pass uses.
    pub fn prune(&mut self, frame: u64) {
        self.groups.retain(|_, group| group.frame + 1 >= frame);
    }

    /// How many groups are currently held.
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    /// Whether no group is held at all.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

impl core::fmt::Debug for UniformGroups {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UniformGroups")
            .field("groups", &self.groups.len())
            .finish()
    }
}
