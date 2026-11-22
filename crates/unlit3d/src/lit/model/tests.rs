use super::*;
use crate::frame::{Frame, dispatch_frame};
use glam::{Mat4, Vec3};

fn node(parent: Option<usize>, translation: Vec3) -> LitModelNode {
    LitModelNode {
        parent,
        transform: Transform {
            translation,
            ..Transform::default()
        },
        primitives: Vec::new(),
        skin: None,
        morph: None,
    }
}

#[test]
fn a_model_places_and_animates_its_own_subtree() {
    // The joint appears after its child to exercise parent-first evaluation.
    let mut child = node(Some(1), Vec3::Y);
    child.skin = Some(LitModelSkin {
        joints: vec![1],
        inverse_bind: vec![Mat4::from_translation(-Vec3::X)],
    });
    child.morph = Some(LitModelMorph {
        weights: vec![0.25, 0.5],
    });
    let model = LitModel::new(
        vec![child, node(None, Vec3::X)],
        vec![LitModelClip {
            name: None,
            duration: 1.0,
            channels: vec![
                LitModelChannel {
                    node: 1,
                    property: LitModelProperty::Translation,
                    interpolation: LitModelInterpolation::Linear,
                    times: vec![0.0, 1.0],
                    values: LitModelChannelValues::Translations(vec![
                        [1.0, 0.0, 0.0],
                        [3.0, 0.0, 0.0],
                    ]),
                },
                LitModelChannel {
                    node: 0,
                    property: LitModelProperty::Weights,
                    interpolation: LitModelInterpolation::Step,
                    times: vec![0.0, 0.5],
                    values: LitModelChannelValues::Weights {
                        targets: 2,
                        values: vec![0.25, 0.5, 0.75, 1.0],
                    },
                },
            ],
        }],
    );
    let mut world = World::new();
    let root = world.root_scope_mut().spawn(LitModelBundle {
        transform: Transform {
            translation: 4.0 * Vec3::X,
            ..Transform::default()
        },
        model: model.clone(),
        animation: LitModelAnimation {
            clip: Some(0),
            time: 0.25,
            ..LitModelAnimation::default()
        },
    });
    let other = world.root_scope_mut().spawn(LitModelBundle::new(model));
    let handles = |world: &World, root| {
        let scope = world.root_scope();
        let mut query = scope.query_one::<&LitModelInstance>(root);
        let instance = query.get();
        (
            instance.nodes[0],
            instance.nodes[1],
            instance.poses[0].unwrap(),
            instance.weights[0].unwrap(),
        )
    };
    let (child, joint, pose, weights) = handles(&world, root);
    assert_eq!(world.children(root).unwrap(), [joint]);
    assert!(world.children(joint).unwrap().contains(&child));
    let check = |world: &World, child_x, pose_x, expected_weights| {
        let scope = world.root_scope();
        let mut query = scope.query_one::<&LitModelInstance>(root);
        assert_eq!(
            query.get().matrices[0].transform_point3(Vec3::ZERO),
            Vec3::new(child_x, 1.0, 0.0)
        );
        assert_eq!(
            scope.query_one::<&SkinPose>(pose).get().matrices,
            [Mat4::from_translation(pose_x * Vec3::X)]
        );
        assert_eq!(
            scope.query_one::<&MorphWeights>(weights).get().weights,
            expected_weights
        );
    };
    check(&world, 5.5, 4.5, [0.25, 0.5]);
    world
        .root_scope()
        .query_one::<&mut Transform>(root)
        .get()
        .translation
        .x = 8.0;
    dispatch_frame(
        &mut world,
        &mut Frame {
            delta_time: 0.25,
            index: 0,
        },
    );
    check(&world, 10.0, 9.0, [0.75, 1.0]);
    LitModelBundle::evaluate(&world.root_scope(), root);
    check(&world, 10.0, 9.0, [0.75, 1.0]);
    world
        .root_scope()
        .query_one::<&mut LitModelAnimation>(root)
        .get()
        .clip = None;
    LitModelBundle::evaluate(&world.root_scope(), root);
    check(&world, 9.0, 8.0, [0.25, 0.5]);

    let (other_child, _, _, other_weights) = handles(&world, other);
    assert_ne!(child, other_child);
    assert_eq!(
        world
            .root_scope()
            .query_one::<&MorphWeights>(other_weights)
            .get()
            .weights,
        [0.25, 0.5]
    );
    world.root_scope_mut().despawn(root).unwrap();
    for entity in [root, child, joint, pose, weights] {
        assert!(world.children(entity).is_err());
    }
    assert!(world.children(other_child).is_ok());
}

#[test]
fn malformed_parent_edges_are_cut_before_evaluation() {
    let nodes = vec![
        node(Some(1), Vec3::X),
        node(Some(0), Vec3::Y),
        node(Some(99), Vec3::Z),
    ];
    assert_eq!(hierarchy(&nodes), [(1, None), (0, Some(1)), (2, None)]);
}
