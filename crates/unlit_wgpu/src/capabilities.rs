//! What the device can do beyond the WebGPU baseline, and how to ask for a
//! narrower device.
//!
//! WebGPU sets a floor every implementation meets, but wgpu also runs on
//! implementations that miss parts of it — WebGL2 most notably, which has no
//! storage buffers, no `base_vertex` and no view formats. Which parts are
//! missing is a property of the *adapter*, reported by
//! [`wgpu::Adapter::get_downlevel_capabilities`]. [`wgpu::Device`] exposes its
//! limits and features but not the downlevel flags, and the adapter is
//! normally dropped once the device is built.
//!
//! So [`DeviceCapabilities`] is captured where the adapter still exists, and
//! carried to where the frame is recorded. It holds only what the device
//! cannot answer for itself: the rest is already visible on the device —
//! whether a storage buffer can be bound is
//! [`supports_storage_buffers`](crate::pipeline::supports_storage_buffers), read
//! from the device's own limits — and asking two sources for one answer is how
//! they drift apart.

use core::fmt;

/// The environment variable that narrows the device to a tier.
const TIER_ENV: &str = "UNLIT3D_DEVICE_TIER";

/// The downlevel capabilities a frame is recorded against.
///
/// See the [module documentation](self) for why this is captured rather than
/// read from the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceCapabilities {
    /// Whether an indexed draw may offset its indices by a non-zero
    /// `base_vertex`.
    base_vertex: bool,
}

impl DeviceCapabilities {
    /// Everything the WebGPU baseline requires.
    ///
    /// What a device assumed rather than asked about gets: the built-in
    /// pipeline then takes its fastest path, which is the only path a
    /// full-featured device needs.
    pub const WEBGPU: Self = Self { base_vertex: true };

    /// What a WebGL2 device has.
    ///
    /// WebGL2 is GLES 3.0, and `base_vertex` needs GLES 3.2, so an indexed draw
    /// there must keep `base_vertex` at zero.
    pub const WEBGL2: Self = Self { base_vertex: false };

    /// Read the capabilities of `adapter`.
    pub fn from_adapter(adapter: &wgpu::Adapter) -> Self {
        let flags = adapter.get_downlevel_capabilities().flags;
        Self {
            base_vertex: flags.contains(wgpu::DownlevelFlags::BASE_VERTEX),
        }
    }

    /// Whether an indexed draw may offset its indices by a non-zero
    /// `base_vertex`.
    ///
    /// Where this is missing, a mesh cannot name its vertices through the
    /// draw's `base_vertex` — the backend would have to call a GL entry point
    /// that does not exist — so its indices are offset at upload instead. The
    /// mesh source is what decides which of the two a mesh's indices get; see
    /// [`MeshInfo`](crate::mesh::MeshInfo) for the addressing the draw then
    /// reads.
    pub fn base_vertex(&self) -> bool {
        self.base_vertex
    }
}

impl Default for DeviceCapabilities {
    fn default() -> Self {
        Self::WEBGPU
    }
}

/// How much of the WebGPU baseline to ask a device for.
///
/// A device is requested with a set of limits, and the limits are what decide
/// most of what the device can do: a WebGL2 device is a WebGPU device asked
/// for
/// [`downlevel_webgl2_defaults`](wgpu::Limits::downlevel_webgl2_defaults).
/// Asking for them therefore reproduces a WebGL2 device's *shape* on hardware
/// that is capable of much more, which is the only way to exercise the paths a
/// browser takes from a machine that has storage buffers and `base_vertex`.
///
/// [`Self::from_env`] reads it from `UNLIT3D_DEVICE_TIER`, so a test run can
/// pick the tier without recompiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceTier {
    /// The adapter's own limits and capabilities. The default.
    #[default]
    Native,
    /// WebGL2's limits and capabilities.
    WebGl2,
}

impl DeviceTier {
    /// Every tier, for a caller that has to enumerate them.
    ///
    /// [`Display`](core::fmt::Display) and [`FromStr`](core::str::FromStr) are inverses over exactly
    /// these, and a test asserts it, so the set cannot grow without the
    /// environment variable learning the new name.
    pub const ALL: &[Self] = &[Self::Native, Self::WebGl2];

    /// The tier named by `UNLIT3D_DEVICE_TIER`, or [`Self::Native`] when it is
    /// unset.
    ///
    /// The value is parsed by [`FromStr`](core::str::FromStr), so an unrecognized one — a
    /// typo like `webgl` — panics rather than silently selecting
    /// [`Self::Native`]. Running the native tier when WebGL2 was asked for is
    /// the failure this variable exists to prevent: it would pass while
    /// exercising none of the paths under test, which is worse than not running.
    ///
    /// # Panics
    ///
    /// If the variable is set to a value that names no tier.
    pub fn from_env() -> Self {
        match std::env::var(TIER_ENV) {
            Ok(value) => value.parse().unwrap_or_else(|()| {
                let names: Vec<_> = Self::ALL.iter().map(Self::to_string).collect();
                panic!(
                    "{TIER_ENV}={value:?} names no device tier; unset it for the adapter's own \
                     limits, or set it to one of {}",
                    names.join(", ")
                )
            }),
            Err(_) => Self::Native,
        }
    }

    /// The limits to request from an adapter whose own limits are
    /// `adapter_limits`.
    ///
    /// A narrowed tier keeps the adapter's texture resolution: the tier's
    /// limits are the *floor* a WebGL2 device meets, and lowering the texture
    /// sizes below what the adapter has would test a device smaller than any
    /// browser ships.
    pub fn limits(self, adapter_limits: &wgpu::Limits) -> wgpu::Limits {
        match self {
            Self::Native => adapter_limits.clone(),
            Self::WebGl2 => {
                wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter_limits.clone())
            }
        }
    }

    /// The capabilities a device of this tier has.
    ///
    /// Independent of the adapter: the tier *is* the set of flags, and a forced
    /// tier has to report what the real device of that tier would.
    pub fn capabilities(self) -> DeviceCapabilities {
        match self {
            Self::Native => DeviceCapabilities::WEBGPU,
            Self::WebGl2 => DeviceCapabilities::WEBGL2,
        }
    }

    /// The capabilities a device of this tier ends up with.
    ///
    /// A narrowed tier is a request, so what it can rely on is the tier's own
    /// capabilities intersected with what the adapter really reports: a forced
    /// WebGL2 tier is only as capable as WebGL2, and an adapter that is even
    /// more limited is only as capable as itself.
    pub fn capabilities_of(self, adapter: &wgpu::Adapter) -> DeviceCapabilities {
        let actual = DeviceCapabilities::from_adapter(adapter);
        match self {
            Self::Native => actual,
            Self::WebGl2 => DeviceCapabilities { base_vertex: false },
        }
    }
}

impl fmt::Display for DeviceTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Native => "native",
            Self::WebGl2 => "webgl2",
        })
    }
}

impl core::str::FromStr for DeviceTier {
    type Err = ();

    /// Parse a tier name, case-insensitively.
    ///
    /// This is the inverse of [`Display`](core::fmt::Display), and the two are written
    /// against the same names so a value printed by one is accepted by the
    /// other.
    fn from_str(name: &str) -> Result<Self, ()> {
        match name {
            _ if name.eq_ignore_ascii_case("native") => Ok(Self::Native),
            _ if name.eq_ignore_ascii_case("webgl2") => Ok(Self::WebGl2),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_webgpu_baseline() {
        assert!(DeviceCapabilities::default().base_vertex());
        assert_eq!(DeviceCapabilities::default(), DeviceCapabilities::WEBGPU);
    }

    #[test]
    fn a_webgl2_device_has_no_base_vertex() {
        assert!(!DeviceCapabilities::WEBGL2.base_vertex());
    }

    #[test]
    fn the_native_tier_leaves_the_adapters_limits_alone() {
        let adapter = wgpu::Limits {
            max_texture_dimension_2d: 4096,
            max_storage_buffers_per_shader_stage: 8,
            ..wgpu::Limits::default()
        };

        let requested = DeviceTier::Native.limits(&adapter);

        assert_eq!(requested.max_texture_dimension_2d, 4096);
        assert_eq!(requested.max_storage_buffers_per_shader_stage, 8);
    }

    #[test]
    fn the_webgl2_tier_has_no_storage_buffers_but_keeps_the_texture_size() {
        let adapter = wgpu::Limits {
            max_texture_dimension_2d: 16384,
            max_storage_buffers_per_shader_stage: 16,
            ..wgpu::Limits::default()
        };

        let requested = DeviceTier::WebGl2.limits(&adapter);

        // What makes a storage-buffer bind-group layout fail outright, which is
        // how the pipeline detects that it has to read its arrays from
        // textures instead.
        assert_eq!(requested.max_storage_buffers_per_shader_stage, 0);
        assert_eq!(requested.max_storage_buffer_binding_size, 0);
        // The floor's own resolution, raised to what the adapter has: a tier
        // must not shrink the textures below any browser's.
        assert_eq!(requested.max_texture_dimension_2d, 16384);
    }

    #[test]
    fn the_webgl2_tier_is_more_limited_than_the_webgpu_one() {
        let adapter = wgpu::Limits {
            max_texture_dimension_2d: 16384,
            ..wgpu::Limits::default()
        };
        let webgl2 = DeviceTier::WebGl2.limits(&adapter);
        let native = DeviceTier::Native.limits(&adapter);

        assert!(webgl2.max_uniform_buffer_binding_size <= native.max_uniform_buffer_binding_size);
        assert!(webgl2.max_bind_groups <= native.max_bind_groups);
        assert!(
            webgl2.max_storage_buffers_per_shader_stage
                <= native.max_storage_buffers_per_shader_stage
        );
    }

    #[test]
    fn the_tier_decides_the_capabilities() {
        assert!(DeviceTier::Native.capabilities().base_vertex());
        assert!(!DeviceTier::WebGl2.capabilities().base_vertex());
        assert_eq!(DeviceTier::default(), DeviceTier::Native);
    }

    #[test]
    fn the_tier_prints_as_the_name_the_environment_variable_takes() {
        assert_eq!(DeviceTier::Native.to_string(), "native");
        assert_eq!(DeviceTier::WebGl2.to_string(), "webgl2");
    }

    #[test]
    fn every_tier_name_round_trips() {
        for &tier in DeviceTier::ALL {
            let name = tier.to_string();
            assert_eq!(name.parse(), Ok(tier), "{name} parses back to its tier");
            assert_eq!(
                name.to_uppercase().parse(),
                Ok(tier),
                "{name} is case-insensitive"
            );
        }
    }

    #[test]
    fn a_name_that_is_no_tier_is_refused() {
        // `webgl` is the typo this exists for: accepting it as the native tier
        // would run every test against the wrong device and still pass.
        for name in ["webgl", "webgl3", "", "native ", "gles"] {
            assert_eq!(
                name.parse::<DeviceTier>(),
                Err(()),
                "{name:?} names no tier"
            );
        }
    }
}
