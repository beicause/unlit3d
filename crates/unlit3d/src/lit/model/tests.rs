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
    child.skin = Some(LitModelSkin::new(
        vec![1],
        vec![Mat4::from_translation(-Vec3::X)],
    ));
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
                    interpolation: LitModelInterpolation::Linear,
                    times: vec![0.0, 1.0],
                    values: LitModelOutput::Translation(vec![[1.0, 0.0, 0.0], [3.0, 0.0, 0.0]]),
                },
                LitModelChannel {
                    node: 0,
                    interpolation: LitModelInterpolation::Step,
                    times: vec![0.0, 0.5],
                    values: LitModelOutput::Weights {
                        targets: 2,
                        values: vec![0.25, 0.5, 0.75, 1.0],
                    },
                },
            ],
        }],
    )
    .expect("the model is well formed");
    let mut world = World::new();
    let root = world.root_scope_mut().spawn(LitModelBundle {
        transform: Transform {
            translation: 4.0 * Vec3::X,
            ..Transform::default()
        },
        model: model.clone(),
        animation: LitModelAnimation {
            clip: Some(0),
            time: AnimationTime::from_secs(0.25),
            ..LitModelAnimation::default()
        },
    });
    let other = world.root_scope_mut().spawn(LitModelBundle::new(model));
    let handles = |world: &World, root| {
        let scope = world.root_scope();
        let mut query = scope.query_one::<&LitModelInstance>(root);
        let instance = query.get();
        (
            instance.nodes()[0],
            instance.nodes()[1],
            instance.poses()[0].unwrap(),
            instance.weights()[0].unwrap(),
        )
    };
    let (child, joint, pose, weights) = handles(&world, root);
    assert_eq!(world.children(root).unwrap(), [joint]);
    assert!(world.children(joint).unwrap().contains(&child));
    let check = |world: &World, child_x, pose_x, expected_weights| {
        let scope = world.root_scope();
        let mut query = scope.query_one::<&LitModelInstance>(root);
        assert_eq!(
            query.get().matrices()[0].transform_point3(Vec3::ZERO),
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
            delta_time: crate::time::FrameDelta::from_secs(0.25),
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
fn explicit_evaluation_samples_without_advancing_time() {
    let model = LitModel::new(
        vec![node(None, Vec3::ZERO)],
        vec![LitModelClip {
            name: None,
            duration: 1.0,
            channels: vec![LitModelChannel {
                node: 0,
                interpolation: LitModelInterpolation::Linear,
                times: vec![0.0, 1.0],
                values: LitModelOutput::Translation(vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]),
            }],
        }],
    )
    .expect("the model is well formed");
    let mut world = World::new();
    let root = world.root_scope_mut().spawn(LitModelBundle {
        transform: Transform::default(),
        model,
        animation: LitModelAnimation {
            clip: Some(0),
            time: AnimationTime::from_secs(1.25),
            ..LitModelAnimation::default()
        },
    });
    LitModelBundle::evaluate(&world.root_scope(), root);
    let scope = world.root_scope();
    assert_eq!(
        scope
            .query_one::<&LitModelAnimation>(root)
            .get()
            .time
            .as_secs(),
        1.25
    );
    let mut instance_query = scope.query_one::<&LitModelInstance>(root);
    let instance = instance_query.get();
    assert_eq!(instance.matrices()[0].transform_point3(Vec3::ZERO).x, 0.25);
}

#[test]
fn a_parent_outside_the_node_list_is_refused() {
    let nodes = vec![
        node(Some(1), Vec3::X),
        node(Some(0), Vec3::Y),
        node(Some(99), Vec3::Z),
    ];
    assert_eq!(
        LitModel::new(nodes, Vec::new()).unwrap_err(),
        LitModelError::ParentOutOfRange {
            node: 2,
            parent: 99
        }
    );
}

#[test]
fn a_parent_cycle_is_refused() {
    // Two nodes naming each other have no evaluation order: whatever order a
    // walk picks, one of them multiplies by a matrix that is not built yet.
    let nodes = vec![node(Some(1), Vec3::X), node(Some(0), Vec3::Y)];
    assert_eq!(
        LitModel::new(nodes, Vec::new()).unwrap_err(),
        LitModelError::ParentCycle { node: 0 }
    );
}

#[test]
fn a_channel_short_of_the_values_its_keys_name_is_refused() {
    let model = LitModel::new(
        vec![node(None, Vec3::ZERO)],
        vec![LitModelClip {
            name: Some("short".to_owned()),
            duration: 1.0,
            channels: vec![LitModelChannel {
                node: 0,
                interpolation: LitModelInterpolation::Linear,
                times: vec![0.0, 1.0],
                values: LitModelOutput::Translation(vec![[0.0, 0.0, 0.0]]),
            }],
        }],
    );
    assert_eq!(
        model.unwrap_err(),
        LitModelError::ChannelValuesShort {
            clip: 0,
            channel: 0,
            times: 2,
            values: 1,
            expected: 2
        }
    );
}

#[test]
fn morph_weights_that_do_not_match_the_node_are_refused() {
    let mut morphed = node(None, Vec3::ZERO);
    morphed.morph = Some(LitModelMorph {
        weights: vec![0.0, 0.0],
    });
    let model = LitModel::new(
        vec![morphed],
        vec![LitModelClip {
            name: None,
            duration: 1.0,
            channels: vec![LitModelChannel {
                node: 0,
                interpolation: LitModelInterpolation::Linear,
                times: vec![0.0, 1.0],
                values: LitModelOutput::Weights {
                    targets: 3,
                    values: vec![0.0; 6],
                },
            }],
        }],
    );
    assert_eq!(
        model.unwrap_err(),
        LitModelError::MorphTargetsMismatch {
            clip: 0,
            channel: 0,
            node: 0,
            targets: 3,
            weights: 2
        }
    );
}

#[test]
fn a_clip_is_found_by_the_name_the_document_gave_it() {
    let model = LitModel::new(
        vec![node(None, Vec3::ZERO)],
        vec![
            LitModelClip {
                name: Some("Idle".to_owned()),
                duration: 1.0,
                channels: Vec::new(),
            },
            LitModelClip {
                name: Some("Walk".to_owned()),
                duration: 2.0,
                channels: Vec::new(),
            },
        ],
    )
    .expect("the model is well formed");
    assert_eq!(model.clip_index("Walk"), Some(1));
    assert_eq!(model.clip_index("Run"), None);
}

#[test]
fn a_manual_clock_is_sampled_only_by_the_caller() {
    let model = LitModel::new(
        vec![node(None, Vec3::ZERO)],
        vec![LitModelClip {
            name: None,
            duration: 1.0,
            channels: vec![LitModelChannel {
                node: 0,
                interpolation: LitModelInterpolation::Linear,
                times: vec![0.0, 1.0],
                values: LitModelOutput::Translation(vec![[0.0, 0.0, 0.0], [2.0, 0.0, 0.0]]),
            }],
        }],
    )
    .expect("the model is well formed");
    let mut world = World::new();
    let root = world.root_scope_mut().spawn(LitModelBundle {
        transform: Transform::default(),
        model,
        animation: LitModelAnimation {
            clip: Some(0),
            time: AnimationTime::from_secs(0.25),
            clock: LitModelClock::Manual,
        },
    });
    let x = |world: &World| {
        world
            .root_scope()
            .query_one::<&LitModelInstance>(root)
            .get()
            .matrices()[0]
            .transform_point3(Vec3::ZERO)
            .x
    };
    // The spawn sampled the time it was written with.
    assert_eq!(x(&world), 0.5);
    // A frame neither advances a manual clock nor re-samples it, so the time
    // written here reaches the draw only through the caller's own pass.
    world
        .root_scope()
        .query_one::<&mut LitModelAnimation>(root)
        .get()
        .time = AnimationTime::from_secs(0.75);
    dispatch_frame(
        &mut world,
        &mut Frame {
            delta_time: crate::time::FrameDelta::from_secs(0.5),
            index: 0,
        },
    );
    assert_eq!(
        world
            .root_scope()
            .query_one::<&LitModelAnimation>(root)
            .get()
            .time
            .as_secs(),
        0.75
    );
    assert_eq!(x(&world), 0.5);
    LitModelBundle::evaluate(&world.root_scope(), root);
    assert_eq!(x(&world), 1.5);
}

#[test]
fn a_base_edit_reaches_a_clip_held_still() {
    // A manual clock whose time does not move leaves sampling with nothing new
    // to read, so the pass may reuse its locals. An edit to the node's base
    // transform is the one change that must still get through.
    let model = LitModel::new(
        vec![node(None, Vec3::ZERO)],
        vec![LitModelClip {
            name: None,
            duration: 1.0,
            channels: vec![LitModelChannel {
                node: 0,
                interpolation: LitModelInterpolation::Linear,
                times: vec![0.0, 1.0],
                values: LitModelOutput::Translation(vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]),
            }],
        }],
    )
    .expect("the model is well formed");
    let mut world = World::new();
    let root = world.root_scope_mut().spawn(LitModelBundle {
        transform: Transform::default(),
        model,
        animation: LitModelAnimation {
            clip: Some(0),
            time: AnimationTime::from_secs(0.5),
            clock: LitModelClock::Manual,
        },
    });
    let child = world
        .root_scope()
        .query_one::<&LitModelInstance>(root)
        .get()
        .nodes()[0];
    // The edit changes the scale, which the translation channel does not drive,
    // so the new scale must survive the resample that the channel forces.
    let point = |world: &World| {
        let scope = world.root_scope();
        let mut query = scope.query_one::<&LitModelInstance>(root);
        query.get().matrices()[0].transform_point3(Vec3::X)
    };
    assert_eq!(point(&world), Vec3::new(1.5, 0.0, 0.0));
    world
        .root_scope()
        .query_one::<&mut LitModelNodeState>(child)
        .get()
        .local = Transform {
        scale: Vec3::splat(2.0),
        ..Transform::default()
    };
    LitModelBundle::evaluate(&world.root_scope(), root);
    assert_eq!(point(&world), Vec3::new(2.5, 0.0, 0.0));
    LitModelBundle::evaluate(&world.root_scope(), root);
    assert_eq!(point(&world), Vec3::new(2.5, 0.0, 0.0));
}

#[test]
fn a_dead_handle_does_not_hold_back_the_rest_of_its_batch() {
    let morph = || {
        Some(LitModelMorph {
            weights: vec![0.25, 0.5],
        })
    };
    let mut first = node(None, Vec3::ZERO);
    first.morph = morph();
    let mut second = node(None, Vec3::X);
    second.morph = morph();
    let model = LitModel::new(vec![first, second], Vec::new()).expect("the model is well formed");
    let mut world = World::new();
    let root = world.root_scope_mut().spawn(LitModelBundle::new(model));
    let (nodes, weights) = {
        let scope = world.root_scope();
        let mut query = scope.query_one::<&LitModelInstance>(root);
        let instance = query.get();
        (
            [instance.nodes()[0], instance.nodes()[1]],
            [
                instance.weights()[0].unwrap(),
                instance.weights()[1].unwrap(),
            ],
        )
    };
    world.root_scope_mut().despawn(weights[0]).unwrap();
    world
        .root_scope()
        .query_one::<&mut MorphWeights>(weights[1])
        .get()
        .weights = vec![9.0, 9.0];
    // Moving the second node forces a resample, which marks every morph for writing.
    world
        .root_scope()
        .query_one::<&mut LitModelNodeState>(nodes[1])
        .get()
        .local
        .translation
        .x = 3.0;
    LitModelBundle::evaluate(&world.root_scope(), root);
    assert_eq!(
        world
            .root_scope()
            .query_one::<&MorphWeights>(weights[1])
            .get()
            .weights,
        [0.25, 0.5]
    );
}

#[test]
fn a_model_of_another_shape_is_reported_rather_than_written_through() {
    // An instance is spawned against one asset, and a caller may then put
    // another model on the same entity. The evaluation indexes the instance by
    // the node count it was built with, so a shorter model has to be refused
    // before any pass writes through it — in a release build too, where a
    // debug assertion would be gone.
    let spawned = LitModel::new(
        vec![node(None, Vec3::ZERO), node(Some(0), Vec3::X)],
        Vec::new(),
    )
    .expect("the model is well formed");
    let shorter =
        LitModel::new(vec![node(None, Vec3::ZERO)], Vec::new()).expect("the model is well formed");
    let mut world = World::new();
    let root = world.root_scope_mut().spawn(LitModelBundle::new(spawned));
    let read = |world: &World| {
        let scope = world.root_scope();
        let mut query = scope.query_one::<&LitModelInstance>(root);
        query.get().matrices().to_vec()
    };
    LitModelBundle::evaluate(&world.root_scope(), root);
    let before = read(&world);
    assert_eq!(before.len(), 2);
    *world.root_scope().query_one::<&mut LitModel>(root).get() = shorter;
    LitModelBundle::evaluate(&world.root_scope(), root);
    dispatch_frame(
        &mut world,
        &mut Frame {
            delta_time: crate::time::FrameDelta::from_secs(0.25),
            index: 0,
        },
    );
    assert_eq!(
        read(&world),
        before,
        "a model of another shape leaves the instance it cannot fill alone",
    );
}
