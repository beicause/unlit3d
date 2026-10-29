//! The device a test run got, described in wgpu's own terms.
//!
//! A snapshot mismatch is usually a difference between one driver and another,
//! so the first thing to establish about a failing run is which driver it ran on:
//! the backend, the adapter, the features and the limits, all of which change what
//! a frame records. The same description is what a passing run leaves behind,
//! because a comparison against a stored frame is only meaningful against the
//! device the stored frame came from.
//!
//! The types are wgpu's and its `serde` feature describes them, so nothing here
//! formats a value by hand: the description is the adapter, features and limits
//! serialized as wgpu already spells them, which is what keeps a printed run and
//! a `wgpu-info` reading comparable.

use serde::Serialize;

/// The device a test run draws with, as a serializable record.
///
/// [`Self::read`] is the whole of it; the structure of the JSON is the field
/// list below.
#[derive(Debug, Serialize)]
pub struct GpuInfo {
    /// The tier the device was requested at: `webgpu`, `webgl2` or `native`.
    ///
    /// A tier is what the run *asked* for, so it is not visible in the device:
    /// a WebGL2-shaped device on a desktop adapter reports the adapter's
    /// features. The limits and the capabilities below are what tell the two
    /// apart.
    pub tier: String,
    /// Whether an indexed draw may use a non-zero `base_vertex`, which is what
    /// WebGL2 lacks.
    ///
    /// wgpu keeps this apart from the device, so it is carried here rather
    /// than derived; see [`DeviceCapabilities`](crate::DeviceCapabilities).
    pub base_vertex: bool,
    /// The adapter: backend, name, vendor, driver and device type.
    pub adapter: wgpu::AdapterInfo,
    /// The features the device was created with.
    pub features: wgpu::Features,
    /// The limits the device was created with.
    pub limits: wgpu::Limits,
}

impl GpuInfo {
    /// Read the device a context holds.
    ///
    /// The tier comes from the context rather than from the device: a device
    /// cannot report which limits were asked for, only what it was given.
    pub fn read(ctx: &crate::Ctx) -> Self {
        Self {
            tier: ctx.tier.to_string(),
            base_vertex: ctx.capabilities.base_vertex(),
            adapter: ctx.device.adapter_info(),
            features: ctx.device.features(),
            limits: ctx.device.limits(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record with values that need no GPU.
    fn sample() -> GpuInfo {
        GpuInfo {
            tier: "webgpu".to_owned(),
            base_vertex: true,
            adapter: wgpu::AdapterInfo::new(wgpu::DeviceType::Cpu, wgpu::Backend::Vulkan),
            features: wgpu::Features::empty(),
            limits: wgpu::Limits::defaults(),
        }
    }

    #[test]
    fn the_description_is_wgpus_own_spelling() {
        let value = serde_json::to_value(sample()).expect("a serializable description");
        let object = value.as_object().expect("an object at the top level");
        let keys: Vec<&String> = object.keys().collect();
        assert_eq!(
            keys,
            ["adapter", "base_vertex", "features", "limits", "tier"],
            "the record's own fields come first, alphabetically by serde_json's map"
        );

        // The limits are wgpu's, whose names are the fields it declares rather
        // than anything this crate chose: every one of them is carried, so a
        // reader gets the same table a `wgpu-info` run prints.
        let limits = object["limits"].as_object().expect("an object of limits");
        let declared =
            serde_json::to_value(wgpu::Limits::defaults()).expect("wgpu's own serialization");
        assert_eq!(limits, declared.as_object().expect("an object of limits"));
        assert!(limits.contains_key("maxTextureDimension2D"));

        // `AdapterInfo`'s fields arrive under their own names too, not under a
        // summary this crate would have had to invent.
        let adapter = object["adapter"].as_object().expect("an object");
        assert!(adapter.contains_key("device_type"));
        assert!(adapter.contains_key("backend"));
    }

    #[test]
    fn an_empty_feature_set_is_an_empty_string() {
        // `Features` is a bitflags set, which serializes as `"A | B"`: the
        // empty set has nothing to join, so it is the empty string rather than
        // an empty list. A reader diffing two runs sees the names themselves.
        let value = serde_json::to_value(wgpu::Features::empty()).expect("serializable");
        assert_eq!(value, serde_json::Value::String(String::new()));
    }

    #[test]
    fn the_tier_is_its_name() {
        for tier in crate::DeviceTier::ALL {
            let info = GpuInfo {
                tier: tier.to_string(),
                ..sample()
            };
            let value = serde_json::to_value(info).expect("serializable");
            assert_eq!(value["tier"], serde_json::Value::String(tier.to_string()));
        }
    }
}
