//! Component codecs: the bridge between an ECS component and JSON.
//!
//! The MCP server reads and writes components by name over JSON-RPC. A
//! component's Rust type is the only thing that knows its fields, so the bridge
//! is a codec registered per type: it encodes a live component into JSON,
//! decodes JSON back into a value, and pushes a decoded value into an
//! [ArchetypeBuilder] for spawn_entity. The registry is ordinary data a host
//! app extends with its own components; nothing about the built-in components is
//! privileged.

use glam::{Mat4, Quat, Vec3, Vec4};
use serde_json::{Value, json};
use unlit_ecs::{ArchetypeBuilder, Entity, World};
use unlit3d::prelude::*;

/// A component that can cross the JSON boundary.
///
/// Implementing this for a type registers it with [ComponentRegistry], which
/// makes it visible to the MCP tools by [Self::NAME]. Encoding is how a live
/// component is reported; decoding is how a JSON value becomes a component the
/// registry can set or spawn.
pub trait ComponentCodec: 'static {
    /// The name the component is addressed by over MCP.
    const NAME: &'static str;

    /// This value as JSON.
    fn encode(&self) -> Value;

    /// A value decoded from JSON, or a message saying why it could not be.
    fn decode(value: &Value) -> Result<Self, String>
    where
        Self: Sized;

    /// The value a partial JSON may omit fields from, if the component has one.
    ///
    /// Spawning merges the given fields over this, so a component with defaults
    /// can be created from only the fields that matter. Without it every field
    /// must be given.
    fn default_value() -> Option<Value> {
        None
    }
}

/// The per-type functions the registry stores.
struct Entry {
    name: &'static str,
    encode: fn(&World, Entity) -> Option<Value>,
    set: fn(&World, Entity, &Value) -> Result<(), String>,
    push: fn(&mut ArchetypeBuilder, &Value) -> Result<(), String>,
}

/// The set of components the MCP tools can read, write and spawn.
pub struct ComponentRegistry {
    entries: Vec<Entry>,
}

impl Default for ComponentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ComponentRegistry {
    /// A registry holding the built-in unlit3d and unlit_ecs codecs.
    pub fn new() -> Self {
        let mut registry = Self {
            entries: Vec::new(),
        };
        registry.register::<Transform>();
        registry.register::<Camera>();
        registry.register::<RenderLoadOps>();
        registry.register::<GpuMesh>();
        registry.register::<GpuMaterial>();
        registry.register::<GpuRenderPipeline<UnlitPipelineKey>>();
        registry.register::<ZSortedDrawing>();
        registry.register::<SkinPose>();
        registry.register::<MorphWeights>();
        registry.register::<SkinBinding>();
        registry.register::<MorphBinding>();
        registry.register::<InstanceColor>();
        registry.register::<InstanceCutoff>();
        registry.register::<InputState>();
        registry
    }

    /// Add C to the registry, replacing any earlier codec with the same name.
    pub fn register<C: ComponentCodec>(&mut self) {
        self.entries.retain(|entry| entry.name != C::NAME);
        self.entries.push(Entry {
            name: C::NAME,
            encode: encode::<C>,
            set: set::<C>,
            push: push::<C>,
        });
    }

    /// Whether a component with this name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|entry| entry.name == name)
    }

    /// The names of every registered component.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.entries.iter().map(|entry| entry.name)
    }

    /// Encode entity's name component, or an error if either is missing.
    pub fn encode(&self, world: &World, entity: Entity, name: &str) -> Result<Value, String> {
        let entry = self.entry(name)?;
        (entry.encode)(world, entity)
            .ok_or_else(|| format!("entity {entity:?} has no {name} component"))
    }

    /// Overwrite entity's name component from value.
    pub fn set(
        &self,
        world: &World,
        entity: Entity,
        name: &str,
        value: &Value,
    ) -> Result<(), String> {
        let entry = self.entry(name)?;
        (entry.set)(world, entity, value)
    }

    /// Push a name component decoded from value into builder.
    pub fn push(
        &self,
        builder: &mut ArchetypeBuilder,
        name: &str,
        value: &Value,
    ) -> Result<(), String> {
        let entry = self.entry(name)?;
        (entry.push)(builder, value)
    }

    fn entry(&self, name: &str) -> Result<&Entry, String> {
        self.entries
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| {
                format!(
                    "unknown component {name:?}; known components: {}",
                    self.known()
                )
            })
    }

    fn known(&self) -> String {
        self.names().collect::<Vec<_>>().join(", ")
    }
}

fn encode<C: ComponentCodec>(world: &World, entity: Entity) -> Option<Value> {
    world.get::<C>(entity).map(|component| component.encode())
}

fn set<C: ComponentCodec>(world: &World, entity: Entity, value: &Value) -> Result<(), String> {
    let base = encode::<C>(world, entity)
        .ok_or_else(|| format!("entity {entity:?} has no {} component", C::NAME))?;
    let decoded = C::decode(&merge(base, value))?;
    world
        .with_mut::<C, _>(entity, |component| *component = decoded)
        .ok_or_else(|| format!("entity {entity:?} has no {} component", C::NAME))
}

/// Overlay patch's fields on base.
///
/// A set that names only some fields keeps the ones it omits, so callers can
/// change one property without restating the whole component. A patch that is
/// not an object replaces the base outright.
fn merge(base: Value, patch: &Value) -> Value {
    let Some(fields) = patch.as_object() else {
        return patch.clone();
    };
    let Value::Object(mut object) = base else {
        return patch.clone();
    };
    for (key, field) in fields {
        object.insert(key.clone(), field.clone());
    }
    Value::Object(object)
}

fn push<C: ComponentCodec>(builder: &mut ArchetypeBuilder, value: &Value) -> Result<(), String> {
    let value = match C::default_value() {
        Some(base) => merge(base, value),
        None => value.clone(),
    };
    builder.push(C::decode(&value)?);
    Ok(())
}

fn f32_at(value: &Value, index: usize, field: &str) -> Result<f32, String> {
    value
        .get(index)
        .and_then(Value::as_f64)
        .map(|number| number as f32)
        .ok_or_else(|| format!("{field} must be an array of numbers"))
}

fn vec3(value: &Value, field: &str) -> Result<Vec3, String> {
    Ok(Vec3::new(
        f32_at(value, 0, field)?,
        f32_at(value, 1, field)?,
        f32_at(value, 2, field)?,
    ))
}

fn vec4(value: &Value, field: &str) -> Result<Vec4, String> {
    Ok(Vec4::new(
        f32_at(value, 0, field)?,
        f32_at(value, 1, field)?,
        f32_at(value, 2, field)?,
        f32_at(value, 3, field)?,
    ))
}

fn quat(value: &Value, field: &str) -> Result<Quat, String> {
    let vec = vec4(value, field)?;
    Ok(Quat::from_xyzw(vec.x, vec.y, vec.z, vec.w))
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, String> {
    value
        .get(name)
        .ok_or_else(|| format!("missing field {name:?}"))
}

fn mat4(value: &Value, name: &str) -> Result<Mat4, String> {
    let array = field(value, name)?;
    let mut columns = [0.0f32; 16];
    for (index, slot) in columns.iter_mut().enumerate() {
        *slot = f32_at(array, index, name)?;
    }
    Ok(Mat4::from_cols_array(&columns))
}

fn entity_from(value: &Value) -> Result<Entity, String> {
    let bits = value.as_u64().ok_or("an entity must be its u64 bits")?;
    Ok(Entity::from_raw(bits as u32, (bits >> 32) as u32))
}

impl ComponentCodec for Transform {
    const NAME: &'static str = "Transform";

    fn encode(&self) -> Value {
        json!({
            "translation": [self.translation.x, self.translation.y, self.translation.z],
            "rotation": [self.rotation.x, self.rotation.y, self.rotation.z, self.rotation.w],
            "scale": [self.scale.x, self.scale.y, self.scale.z],
        })
    }

    fn decode(value: &Value) -> Result<Self, String> {
        Ok(Self {
            translation: vec3(field(value, "translation")?, "translation")?,
            rotation: quat(field(value, "rotation")?, "rotation")?,
            scale: vec3(field(value, "scale")?, "scale")?,
        })
    }

    fn default_value() -> Option<Value> {
        Some(Self::default().encode())
    }
}

impl ComponentCodec for Camera {
    const NAME: &'static str = "Camera";

    fn encode(&self) -> Value {
        json!({
            "view_from_world": self.view_from_world.to_cols_array(),
            "clip_from_view": self.clip_from_view.to_cols_array(),
            "active": self.active,
        })
    }

    fn decode(value: &Value) -> Result<Self, String> {
        Ok(Self {
            view_from_world: mat4(value, "view_from_world")?,
            clip_from_view: mat4(value, "clip_from_view")?,
            active: field(value, "active")?
                .as_bool()
                .ok_or("active must be a boolean")?,
        })
    }
}

impl ComponentCodec for RenderLoadOps {
    const NAME: &'static str = "RenderLoadOps";

    fn encode(&self) -> Value {
        let color = match &self.color {
            wgpu::LoadOp::Clear(color) => json!({"clear": [color.r, color.g, color.b, color.a]}),
            wgpu::LoadOp::Load => json!({"load": true}),
            _ => json!("unknown"),
        };
        let depth = match &self.depth {
            wgpu::LoadOp::Clear(value) => json!({"clear": value}),
            wgpu::LoadOp::Load => json!({"load": true}),
            _ => json!("unknown"),
        };
        let stencil = match &self.stencil {
            wgpu::LoadOp::Clear(value) => json!({"clear": value}),
            wgpu::LoadOp::Load => json!({"load": true}),
            _ => json!("unknown"),
        };
        json!({"color": color, "depth": depth, "stencil": stencil})
    }

    fn decode(value: &Value) -> Result<Self, String> {
        fn decode_color(value: &Value) -> Result<wgpu::LoadOp<wgpu::Color>, String> {
            if let Some(array) = value.get("clear") {
                return Ok(wgpu::LoadOp::Clear(wgpu::Color {
                    r: f32_at(array, 0, "color")? as f64,
                    g: f32_at(array, 1, "color")? as f64,
                    b: f32_at(array, 2, "color")? as f64,
                    a: f32_at(array, 3, "color")? as f64,
                }));
            }
            Ok(wgpu::LoadOp::Load)
        }
        fn decode_scalar<T: Copy>(
            value: &Value,
            number: impl FnOnce(&Value) -> Result<T, String>,
        ) -> Result<wgpu::LoadOp<T>, String> {
            match value.get("clear") {
                Some(clear) => Ok(wgpu::LoadOp::Clear(number(clear)?)),
                None => Ok(wgpu::LoadOp::Load),
            }
        }
        // Each operation is independent, so a value that names only some of
        // them leaves the rest loading, which is what "no clear" means.
        let load_color = json!({"load": true});
        let load_scalar = json!({"load": true});
        Ok(Self {
            color: decode_color(value.get("color").unwrap_or(&load_color))?,
            depth: decode_scalar(value.get("depth").unwrap_or(&load_scalar), |clear| {
                clear
                    .as_f64()
                    .map(|n| n as f32)
                    .ok_or_else(|| "depth clear must be a number".to_string())
            })?,
            stencil: decode_scalar(value.get("stencil").unwrap_or(&load_scalar), |clear| {
                clear
                    .as_u64()
                    .map(|n| n as u32)
                    .ok_or_else(|| "stencil clear must be an integer".to_string())
            })?,
        })
    }
}

impl ComponentCodec for GpuMesh {
    const NAME: &'static str = "GpuMesh";

    fn encode(&self) -> Value {
        json!({
            "count": self.count,
            "first": self.first,
            "base_vertex": self.base_vertex,
            "indexed": self.indexed,
            "morph_targets": self.morph_targets,
            "skinned": self.skinned,
            "aabb": {
                "center": [self.aabb.center.x, self.aabb.center.y, self.aabb.center.z],
                "half_extents": [self.aabb.half_extents.x, self.aabb.half_extents.y, self.aabb.half_extents.z],
            },
        })
    }

    fn decode(_value: &Value) -> Result<Self, String> {
        Err("GpuMesh is read-only; allocate one with allocate_unlit_mesh".to_string())
    }
}

impl ComponentCodec for GpuMaterial {
    const NAME: &'static str = "GpuMaterial";

    fn encode(&self) -> Value {
        json!({"bind_group": self.bind_group_id.index()})
    }

    fn decode(_value: &Value) -> Result<Self, String> {
        Err("GpuMaterial is read-only; allocate one with allocate_unlit_material".to_string())
    }
}

impl ComponentCodec for GpuRenderPipeline<UnlitPipelineKey> {
    const NAME: &'static str = "UnlitPipeline";

    fn encode(&self) -> Value {
        json!({"kind": "unlit"})
    }

    fn decode(_value: &Value) -> Result<Self, String> {
        Err("a pipeline is read-only; it is created with a mesh".to_string())
    }
}

impl ComponentCodec for ZSortedDrawing {
    const NAME: &'static str = "ZSortedDrawing";

    fn encode(&self) -> Value {
        json!({})
    }

    fn decode(_value: &Value) -> Result<Self, String> {
        Ok(Self)
    }
}

impl ComponentCodec for SkinPose {
    const NAME: &'static str = "SkinPose";

    fn encode(&self) -> Value {
        json!({
            "matrices": self.matrices.iter().map(|matrix| matrix.to_cols_array()).collect::<Vec<_>>(),
        })
    }

    fn decode(value: &Value) -> Result<Self, String> {
        let matrices = field(value, "matrices")?
            .as_array()
            .ok_or("matrices must be an array")?
            .iter()
            .map(|matrix| {
                let mut columns = [0.0f32; 16];
                for (index, slot) in columns.iter_mut().enumerate() {
                    *slot = f32_at(matrix, index, "matrices")?;
                }
                Ok(Mat4::from_cols_array(&columns))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self { matrices })
    }
}

impl ComponentCodec for MorphWeights {
    const NAME: &'static str = "MorphWeights";

    fn encode(&self) -> Value {
        json!({"weights": self.weights})
    }

    fn decode(value: &Value) -> Result<Self, String> {
        let weights = field(value, "weights")?
            .as_array()
            .ok_or("weights must be an array")?
            .iter()
            .map(|weight| {
                weight
                    .as_f64()
                    .map(|n| n as f32)
                    .ok_or_else(|| "a weight must be a number".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { weights })
    }
}

impl ComponentCodec for SkinBinding {
    const NAME: &'static str = "SkinBinding";

    fn encode(&self) -> Value {
        json!({"pose": self.pose.to_bits()})
    }

    fn decode(value: &Value) -> Result<Self, String> {
        Ok(Self {
            pose: entity_from(field(value, "pose")?)?,
        })
    }
}

impl ComponentCodec for MorphBinding {
    const NAME: &'static str = "MorphBinding";

    fn encode(&self) -> Value {
        json!({"weights": self.weights.to_bits()})
    }

    fn decode(value: &Value) -> Result<Self, String> {
        Ok(Self {
            weights: entity_from(field(value, "weights")?)?,
        })
    }
}

impl ComponentCodec for InstanceColor {
    const NAME: &'static str = "InstanceColor";

    fn encode(&self) -> Value {
        json!({"color": [self.color.x, self.color.y, self.color.z, self.color.w]})
    }

    fn decode(value: &Value) -> Result<Self, String> {
        Ok(Self {
            color: vec4(field(value, "color")?, "color")?,
        })
    }
}

impl ComponentCodec for InstanceCutoff {
    const NAME: &'static str = "InstanceCutoff";

    fn encode(&self) -> Value {
        json!({"cutoff": self.cutoff})
    }

    fn decode(value: &Value) -> Result<Self, String> {
        Ok(Self {
            cutoff: field(value, "cutoff")?
                .as_f64()
                .map(|n| n as f32)
                .ok_or("cutoff must be a number")?,
        })
    }
}

impl ComponentCodec for InputState {
    const NAME: &'static str = "InputState";

    fn encode(&self) -> Value {
        json!({
            "pointer": self.pointer,
            "cursor": self.cursor,
            "pointer_down": self.pointer_down,
            "buttons": self.buttons.bits(),
            "focused": self.focused,
            "size_px": [self.size_px.0, self.size_px.1],
            "scale_factor": self.scale_factor,
            "touches": self.touches.iter().map(|(id, position)| json!([id, position])).collect::<Vec<_>>(),
        })
    }

    fn decode(_value: &Value) -> Result<Self, String> {
        Err("InputState is read-only; send events with send_input".to_string())
    }
}
