#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]

use std::sync::Once;

pub use unlit_wgpu::capabilities::{DeviceCapabilities, DeviceTier};

/// The logging facade the tests record through, re-exported so a test crate
/// needs no `log` dependency of its own.
///
/// Recording is what the harness's backend reports; see [`init_logging`].
pub use log;

// The registry a test file declares its tests in, and the two runners that
// drive it: `native` under `cargo nextest`, `browser` in a wasm build. A test
// file names only the macros, so which runner is in play is not its concern.
mod registry;

pub use registry::{TestBody, TestEntry, TestFn};

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::run;

// The proxy a native build runs in the browser's stead, sending each test to
// the runner over HTTP. Which of the two the binary is, is decided at run time
// from the environment.
#[cfg(not(target_arch = "wasm32"))]
mod proxy;
#[cfg(not(target_arch = "wasm32"))]
pub use proxy::{WASM_TEST_ENV, run_wasm};

#[cfg(target_arch = "wasm32")]
pub mod browser;

/// Drive a future to completion on the calling thread.
///
/// The harness's own entry points already do this, so this is for a
/// synchronous caller that has to build a [`Ctx`] itself. It cannot be used on
/// the web, where a browser tab has nothing to block on.
#[cfg(not(target_arch = "wasm32"))]
pub use pollster::block_on;

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Install the harness's logger backend once, for the whole test process.
///
/// Called by [`Ctx::headless`], so a test that builds a context logs without
/// asking.  Tests that never build a context — the ECS ones — call this
/// directly if they log.
///
/// `RUST_LOG` picks the level natively; without it the default is `warn`.
/// A process holds one logger, so repeated calls are no-ops — and a test that
/// installed a backend of its own first keeps it.
pub fn init_logging() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        #[cfg(not(target_arch = "wasm32"))]
        {
            // `try_init`, not `init`: installing over a backend that is already
            // there would panic, and that backend is the one to keep.
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
                .try_init()
                .ok();
        }
        #[cfg(target_arch = "wasm32")]
        {
            // The browser console is the terminal here.
            console_log::init_with_level(log::Level::Warn).ok();
        }
    });
}

// ---------------------------------------------------------------------------
// GPU context
// ---------------------------------------------------------------------------

/// A ready-to-use GPU context.
pub struct Ctx {
    /// The device every resource in a test is created on.
    pub device: wgpu::Device,
    /// The queue a test's writes and submissions go through.
    pub queue: wgpu::Queue,
    /// What the device can do beyond the WebGPU baseline.
    ///
    /// A [`DeviceCapabilities`] rather than the adapter it was read from: the
    /// adapter is dropped once the device exists, and the renderer needs the
    /// capabilities, not the adapter.
    pub capabilities: DeviceCapabilities,
}

impl Ctx {
    /// Create a headless GPU context with the default adapter.
    ///
    /// `UNLIT3D_DEVICE_TIER` narrows the device; see [`Ctx::headless_for`].
    ///
    /// Asynchronous because wgpu's adapter and device requests are: on the web
    /// they wrap promises that only settle in a later task, and a browser has
    /// no second thread to block on. A synchronous caller drives this through
    /// [`block_on`].
    pub async fn headless() -> Ctx {
        Ctx::headless_for(DeviceTier::from_env()).await
    }

    /// Create a headless GPU context restricted to `tier`.
    ///
    /// The tier's limits are requested from the adapter and its capabilities
    /// are recorded beside the device, so a test can exercise the paths a
    /// WebGL2 browser takes — no storage buffers, no `base_vertex` — on
    /// hardware that has both. The adapter's own limits and capabilities are
    /// what [`DeviceTier::Native`] asks for; the WebGPU baseline's, which is
    /// what [`Ctx::headless`] uses, are what [`DeviceTier::WebGpu`] asks for.
    ///
    /// Nothing is presented: the context is headless on the web too, where the
    /// canvas exists only because a browser will not hand out an adapter
    /// without one.
    pub async fn headless_for(tier: DeviceTier) -> Ctx {
        init_logging();
        let instance = new_instance();

        // A browser creates a GL context from a canvas, so its adapter request
        // has to name a surface even though nothing is ever drawn into it. The
        // surface is dropped once the adapter has been chosen: the device
        // outlives the surface it was chosen for.
        #[cfg(target_arch = "wasm32")]
        let surface = Some(create_surface(&instance));
        #[cfg(not(target_arch = "wasm32"))]
        let surface: Option<wgpu::Surface<'static>> = None;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: surface.as_ref(),
                ..Default::default()
            })
            .await
            .expect("no graphics adapter available");
        let capabilities = tier.capabilities_of(&adapter);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("unlit_wgpu_test_util"),
                required_features: wgpu::Features::empty(),
                required_limits: tier.limits(&adapter.limits()),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("failed to request device");
        drop(surface);
        Ctx {
            device,
            queue,
            capabilities,
        }
    }
}

/// The instance a context's device comes from.
///
/// Natively the environment chooses, so `WGPU_BACKEND` selects a backend the
/// same way it does in an application — which is how the GLES and WebGL2 paths
/// are reached on a machine whose default is Vulkan.
fn new_instance() -> wgpu::Instance {
    #[cfg(not(target_arch = "wasm32"))]
    {
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
    }
    #[cfg(target_arch = "wasm32")]
    {
        // A browser has no environment to read, and WebGL2 is what the wasm
        // suite is for, so the backend is named rather than discovered.
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::GL;
        // WebGL cannot report a fence as done on its own, so a `poll(Wait)`
        // would never resolve: `AutoFinish` is what makes the backend finish
        // the fence itself and let a readback complete. See wgpu#4589.
        descriptor.backend_options.gl.fence_behavior = wgpu::GlFenceBehavior::AutoFinish;
        wgpu::Instance::new(descriptor)
    }
}

/// A canvas for the browser to make a WebGL context from.
///
/// The canvas is never attached to the document and never sized: the context
/// is what the adapter request needs, not the drawing surface.
#[cfg(target_arch = "wasm32")]
fn create_surface(instance: &wgpu::Instance) -> wgpu::Surface<'static> {
    use wasm_bindgen::JsCast as _;

    let canvas = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.create_element("canvas").ok())
        .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        .expect("a document to create the test canvas in");
    instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .expect("a surface from the test canvas")
}

/// Pair a binding slot with a resource.
pub fn bg_entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}

/// Decode an sRGB-encoded byte to linear light.
pub fn srgb_to_linear_u8(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

// ---------------------------------------------------------------------------
// Offscreen colour target and frame readback
// ---------------------------------------------------------------------------

/// An offscreen RGBA8 colour target plus its default view.
///
/// The views are bare wgpu ones: this crate is the layer *below*
/// `unlit_wgpu` — which depends on it, not the other way round — so
/// `unlit_wgpu::resources::TextureView` is not reachable here. A caller that
/// registers this target's view in a resource graph pairs it with its format
/// itself.
#[expect(
    clippy::disallowed_types,
    reason = "this crate sits below unlit_wgpu, which depends on it, so the \
              wrapper type is not reachable here"
)]
pub struct ColorTarget {
    /// The texture the target draws into.
    pub texture: wgpu::Texture,
    /// The view passes are opened over.
    pub view: wgpu::TextureView,
}

#[expect(
    clippy::disallowed_methods,
    reason = "this crate sits below unlit_wgpu, which depends on it, so the \
              wrapper's constructor is not reachable here"
)]
impl ColorTarget {
    /// `Rgba8UnormSrgb` target sized `(width, height)`.
    pub fn new(device: &wgpu::Device, label: &str, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }
}

/// A read-back frame: tight RGBA8 bytes plus dimensions.
pub struct Frame {
    /// The frame's pixels, row-major and tightly packed, four bytes per texel.
    pub rgba: Vec<u8>,
    /// The frame's width, in texels.
    pub width: u32,
    /// The frame's height, in texels.
    pub height: u32,
}

impl core::ops::Deref for Frame {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.rgba
    }
}

impl Frame {
    /// The texel at `(x, y)`, as RGBA8.
    ///
    /// # Panics
    ///
    /// If the coordinates fall outside the frame.
    pub fn pixel_u8(&self, x: u32, y: u32) -> [u8; 4] {
        let off = ((y as usize * self.width as usize) + x as usize) * 4;
        [
            self.rgba[off],
            self.rgba[off + 1],
            self.rgba[off + 2],
            self.rgba[off + 3],
        ]
    }
}

/// Opaque sRGB clear/draw colour from RGB components (alpha = 1).
pub fn rgb(r: f64, g: f64, b: f64) -> wgpu::Color {
    wgpu::Color { r, g, b, a: 1.0 }
}

/// Bytes per texel block of `texture`'s format.
pub fn texel_bytes(texture: &wgpu::Texture) -> u32 {
    texture
        .format()
        .block_copy_size(None)
        .expect("texture format has a block copy size")
}

/// Count pixels whose colour differs from `background` beyond `tolerance`.
pub fn count_pixels_off_background(px: &[u8], background: [f64; 3], tolerance: u8) -> usize {
    px.as_chunks::<4>()
        .0
        .iter()
        .filter(|p| {
            p[..3]
                .iter()
                .zip(background)
                .any(|(&c, bg)| (c as f64 / 255.0 - bg).abs() * 255.0 > tolerance as f64)
        })
        .count()
}

// ---------------------------------------------------------------------------
// Snapshot helpers (require the `snapshot` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "snapshot")]
mod snapshot;

#[cfg(feature = "snapshot")]
pub use snapshot::{
    DEFAULT_MIN_SCORE, DEFAULT_TOLERANCE, Snapshot, SnapshotError, Tolerance,
    assert_image_snapshot, assert_image_snapshot_with_tolerance,
};

// The storing half of the snapshot helpers needs a filesystem, which a browser
// has not got. What is left on the web still compares against the frames
// embedded by `snapshot!`.
#[cfg(all(feature = "snapshot", not(target_arch = "wasm32")))]
pub use snapshot::{
    DEFAULT_MISMATCH_DIR, MISMATCH_DIR_ENV, score_frame_webp, snapshot_path, store_frame_webp,
};

// The encoder is the one piece of the storing half a browser needs: a frame that
// fails a comparison is encoded there and handed to the runner, which has the
// filesystem this side has not got.
#[cfg(all(feature = "snapshot", target_arch = "wasm32"))]
pub use snapshot::encode_frame_webp;
