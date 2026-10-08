//! Reflection support for the components that cross a JSON boundary.
//!
//! [facet] derives a component's own field list, and the
//! [registry](crate::reflect::registry) turns that reflection into the codec a
//! JSON bridge addresses components by.
//!
//! [facet] reflects a type's own fields, but glam's vector and matrix types and
//! [unlit_ecs::Entity] implement neither [facet::Facet] nor serde, and the
//! orphan rule keeps that from being fixed here. A field whose type cannot be
//! reflected is therefore marked `#[facet(opaque, proxy = ...)]` and names a
//! proxy from this module: a plain tuple struct over an array or an integer
//! that `facet` can reflect, with a `From` in each direction to the real
//! value.
//!
//! These are the shapes the JSON bridge spells a value as: a vector is its
//! components, a matrix is its columns in column-major order, and an entity is
//! its [`Entity::to_bits`](unlit_ecs::Entity::to_bits) integer.

pub mod proxies;
pub mod registry;

pub use proxies::{
    AabbProxy, ColorLoadOpProxy, DepthLoadOpProxy, EntityProxy, GpuMaterialProxy, GpuMeshProxy,
    GpuRenderPipelineProxy, InputStateProxy, Mat4Proxy, Mat4VecProxy, QuatProxy,
    StencilLoadOpProxy, Vec3Proxy, Vec4Proxy,
};
pub use registry::{ComponentEntry, contains, encode, entries, entry, names, push, set};

#[cfg(test)]
mod tests {
    use facet::Facet;
    use glam::{Mat4, Quat, Vec3, Vec4};
    use unlit_ecs::Entity;

    use super::*;
    use crate::components::{Camera, MorphBinding, MorphWeights, SkinBinding, SkinPose, Transform};

    /// Every proxy is the real value in both directions, so reflecting a field
    /// through one loses nothing.
    #[test]
    fn a_proxy_round_trips_its_value() {
        let vector = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(Vec3::from(Vec3Proxy::from(&vector)), vector);
        let color = Vec4::new(0.1, 0.2, 0.3, 0.4);
        assert_eq!(Vec4::from(Vec4Proxy::from(&color)), color);
        let rotation = Quat::from_rotation_y(1.5);
        assert_eq!(Quat::from(QuatProxy::from(&rotation)), rotation);
        let matrix = Mat4::from_scale(Vec3::splat(2.0));
        assert_eq!(Mat4::from(Mat4Proxy::from(&matrix)), matrix);
        let matrices = vec![Mat4::IDENTITY, matrix];
        assert_eq!(Vec::<Mat4>::from(Mat4VecProxy::from(&matrices)), matrices);
        let entity = Entity::from_raw(7, 3);
        assert_eq!(Entity::from(EntityProxy::from(&entity)), entity);
    }

    /// The components that cross the JSON boundary derive the reflection, which
    /// is what makes their own field list the codec. The feature is only useful
    /// if the derive is actually there.
    #[test]
    fn the_components_derive_their_reflection() {
        assert_eq!(Transform::SHAPE.type_identifier, "Transform");
        assert_eq!(Camera::SHAPE.type_identifier, "Camera");
        assert_eq!(SkinPose::SHAPE.type_identifier, "SkinPose");
        assert_eq!(MorphWeights::SHAPE.type_identifier, "MorphWeights");
        assert_eq!(SkinBinding::SHAPE.type_identifier, "SkinBinding");
        assert_eq!(MorphBinding::SHAPE.type_identifier, "MorphBinding");
        assert_eq!(
            crate::components::ZSortedDrawing::SHAPE.type_identifier,
            "ZSortedDrawing"
        );
        assert_eq!(
            crate::unlit::InstanceColor::SHAPE.type_identifier,
            "InstanceColor"
        );
        assert_eq!(
            crate::unlit::InstanceCutoff::SHAPE.type_identifier,
            "InstanceCutoff"
        );
    }

    /// The frame's events are part of what a reader reads: a state alone
    /// cannot say which key went down, so the reflection carries them too.
    #[test]
    fn the_input_state_reflects_its_events() {
        use crate::input::{InputEvent, InputState, Key, KeyEvent, Modifiers};
        use crate::reflect::registry;

        let key = InputEvent::Key(KeyEvent {
            key: Key::A,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        });
        let mut world = unlit_ecs::World::new();
        let entity = world.spawn((InputState::default(),));
        let _ = world.with_mut::<InputState, _>(entity, |state| {
            state.push(key.clone());
            state.push(InputEvent::FocusChanged(false));
        });

        let encoded =
            registry::encode(&world, entity, "InputState").expect("the input state encodes");
        let decoded: InputStateProxy =
            facet_json::from_str(&encoded).expect("the encoding is JSON");
        assert_eq!(decoded.events, vec![key, InputEvent::FocusChanged(false)]);
        assert!(
            !decoded.focused,
            "the state the events left behind is read too"
        );
    }

    /// The reflection names the module the type is defined in, which is what
    /// tells two same-named components in different modules apart.
    #[test]
    fn the_reflection_names_the_defining_module() {
        let shape = Transform::SHAPE;
        assert_eq!(shape.module_path, Some("unlit3d::components"));
        assert_eq!(
            format!(
                "{}::{}",
                shape.module_path.expect("a module"),
                shape.type_identifier
            ),
            "unlit3d::components::Transform"
        );
    }
}
