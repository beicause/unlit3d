//! Sampling for both authored models and decoded glTF clips.

use super::model::{LitModelChannel, LitModelInterpolation as Interpolation, LitModelOutput};
use crate::components::Transform;

/// How many values a keyframe stores under an interpolation.
///
/// A cubic spline stores three — in-tangent, value, out-tangent — and every
/// other interpolation stores one.
pub(crate) fn interpolation_stride(interpolation: Interpolation) -> usize {
    match interpolation {
        Interpolation::CubicSpline => 3,
        Interpolation::Step | Interpolation::Linear => 1,
    }
}

/// The keyframe interval `time` falls in, clamped to the keyframes' range.
///
/// Returns the left frame's index, always in `0..times.len() - 1` so a caller
/// can read both ends of the interval, and the fraction of the way through it.
/// Validation guarantees a sampler at least two keyframes, so an empty `times`
/// only reaches a hand-built document; the clamp keeps it from panicking.
pub(crate) fn keyframe(times: &[f32], time: f32) -> (usize, f32) {
    let Some(last) = times.len().checked_sub(1) else {
        return (0, 0.0);
    };
    if time <= times[0] {
        return (0, 0.0);
    }
    if time >= times[last] {
        return (last.saturating_sub(1), 1.0);
    }
    let frame = times
        .partition_point(|&key| key <= time)
        .saturating_sub(1)
        .min(last.saturating_sub(1));
    let span = times[frame + 1] - times[frame];
    let t = if span > 0.0 {
        (time - times[frame]) / span
    } else {
        0.0
    };
    (frame, t)
}

/// The keyframe a step interpolation holds at `time`: the latest one at or
/// before it, so the value jumps when `time` reaches the next.
fn step_frame(times: &[f32], time: f32) -> usize {
    times.partition_point(|&key| key <= time).saturating_sub(1)
}

/// A glTF `translation` or `scale` sampled at `time`.
pub(crate) fn sample_vec3(
    interpolation: Interpolation,
    times: &[f32],
    values: &[[f32; 3]],
    time: f32,
) -> [f32; 3] {
    let stride = interpolation_stride(interpolation);
    match interpolation {
        Interpolation::Step => values[step_frame(times, time) * stride],
        Interpolation::Linear => {
            let (frame, t) = keyframe(times, time);
            glam::Vec3::from(values[frame])
                .lerp(glam::Vec3::from(values[frame + 1]), t)
                .to_array()
        }
        Interpolation::CubicSpline => {
            let (frame, t) = keyframe(times, time);
            cubic(
                glam::Vec3::from(values[frame * 3 + 1]),
                glam::Vec3::from(values[frame * 3 + 2]),
                glam::Vec3::from(values[(frame + 1) * 3 + 1]),
                glam::Vec3::from(values[(frame + 1) * 3]),
                t,
                times[frame + 1] - times[frame],
            )
            .to_array()
        }
    }
}

/// A glTF `rotation` sampled at `time`, always a unit quaternion.
///
/// A linear rotation blends spherically, the spec's own requirement; a cubic
/// one interpolates each component and renormalizes.
pub(crate) fn sample_rotation(
    interpolation: Interpolation,
    times: &[f32],
    values: &[[f32; 4]],
    time: f32,
) -> glam::Quat {
    let stride = interpolation_stride(interpolation);
    match interpolation {
        Interpolation::Step => quat(values[step_frame(times, time) * stride]),
        Interpolation::Linear => {
            let (frame, t) = keyframe(times, time);
            quat(values[frame]).slerp(quat(values[frame + 1]), t)
        }
        Interpolation::CubicSpline => {
            let (frame, t) = keyframe(times, time);
            let value = cubic(
                glam::Vec4::from(values[frame * 3 + 1]),
                glam::Vec4::from(values[frame * 3 + 2]),
                glam::Vec4::from(values[(frame + 1) * 3 + 1]),
                glam::Vec4::from(values[(frame + 1) * 3]),
                t,
                times[frame + 1] - times[frame],
            );
            quat(value.to_array())
        }
    }
}

/// A glTF `weights` channel sampled at `time`, one weight per morph target,
/// written into `out` (which is cleared first) rather than returned.
///
/// A cubic keyframe groups its tangents and values kind-first — every
/// in-tangent, then every value, then every out-tangent — which is a different
/// layout from the one the node channels use.
pub(crate) fn sample_weights(
    interpolation: Interpolation,
    times: &[f32],
    values: &[f32],
    targets: usize,
    time: f32,
    out: &mut Vec<f32>,
) {
    out.clear();
    match interpolation {
        Interpolation::Step => {
            let frame = step_frame(times, time);
            out.extend_from_slice(&values[frame * targets..(frame + 1) * targets]);
        }
        Interpolation::Linear => {
            let (frame, t) = keyframe(times, time);
            let a = &values[frame * targets..(frame + 1) * targets];
            let b = &values[(frame + 1) * targets..(frame + 2) * targets];
            out.extend(a.iter().zip(b).map(|(&a, &b)| a + (b - a) * t));
        }
        Interpolation::CubicSpline => {
            let (frame, t) = keyframe(times, time);
            let span = times[frame + 1] - times[frame];
            let base = frame * 3 * targets;
            let next = (frame + 1) * 3 * targets;
            out.extend((0..targets).map(|target| {
                cubic(
                    values[base + targets + target],
                    values[base + 2 * targets + target],
                    values[next + targets + target],
                    values[next + target],
                    t,
                    span,
                )
            }));
        }
    }
}

/// A value the cubic Hermite formula applies to: one weight, or a vector of
/// them.
///
/// The formula is the same for a scalar and for a vector, so it is written once
/// over this trait rather than once per component count.
trait Hermite: Copy + std::ops::Mul<f32, Output = Self> + std::ops::Add<Output = Self> {}

impl Hermite for f32 {}
impl Hermite for glam::Vec3 {}
impl Hermite for glam::Vec4 {}

/// One segment of a cubic Hermite spline, the spec's `CUBICSPLINE` formula.
fn cubic<H: Hermite>(v0: H, b0: H, v1: H, a1: H, t: f32, span: f32) -> H {
    let t2 = t * t;
    let t3 = t2 * t;
    v0 * (2.0 * t3 - 3.0 * t2 + 1.0)
        + b0 * (span * (t3 - 2.0 * t2 + t))
        + v1 * (-2.0 * t3 + 3.0 * t2)
        + a1 * (span * (t3 - t2))
}

/// A unit quaternion from the `[x, y, z, w]` a glTF rotation stores, falling
/// back to the identity when the value is degenerate.
fn quat(value: [f32; 4]) -> glam::Quat {
    let quat = glam::Quat::from_xyzw(value[0], value[1], value[2], value[3]);
    let length = quat.length();
    if length.is_finite() && length > f32::EPSILON {
        quat / length
    } else {
        glam::Quat::IDENTITY
    }
}

/// Sample a channel into the node local and weight buffer it drives.
///
/// [`LitModel::new`](super::LitModel::new) refuses a channel whose key times and
/// values do not agree with its interpolation and target count, so this reads
/// the values its keys name without a length guard: the only channel that can
/// be short here is one built by hand, out of a model that was never checked.
pub(super) fn sample_channel(
    channel: &LitModelChannel,
    time: f32,
    local: &mut Transform,
    weights: &mut Vec<f32>,
) {
    let mode = channel.interpolation;
    let times = &channel.times;
    // A cubic keyframe stores its value in the middle of three slots.
    let value = usize::from(mode == Interpolation::CubicSpline);
    let single = times.len() == 1;
    match &channel.values {
        LitModelOutput::Translation(values) => {
            local.translation = if single {
                values[value]
            } else {
                sample_vec3(mode, times, values, time)
            }
            .into();
        }
        LitModelOutput::Scale(values) => {
            local.scale = if single {
                values[value]
            } else {
                sample_vec3(mode, times, values, time)
            }
            .into();
        }
        LitModelOutput::Rotation(values) => {
            local.rotation = if single {
                quat(values[value])
            } else {
                sample_rotation(mode, times, values, time)
            };
        }
        LitModelOutput::Weights { targets, values } => {
            if single {
                weights.clear();
                weights.extend_from_slice(&values[value * targets..(value + 1) * targets]);
            } else {
                sample_weights(mode, times, values, *targets, time, weights);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Interpolation, keyframe, sample_rotation, sample_vec3, sample_weights, step_frame,
    };

    /// Asserts a keyframe pair matches, allowing for the float fraction.
    fn assert_keyframe(actual: (usize, f32), expected: (usize, f32)) {
        assert_eq!(actual.0, expected.0, "{actual:?} vs {expected:?}");
        assert!(
            (actual.1 - expected.1).abs() < 1e-6,
            "{actual:?} vs {expected:?}"
        );
    }

    #[test]
    fn a_keyframe_clamps_past_either_end_and_splits_the_span() {
        let times = [0.0, 1.0, 3.0];
        assert_keyframe(keyframe(&times, -1.0), (0, 0.0));
        assert_keyframe(keyframe(&times, 0.0), (0, 0.0));
        assert_keyframe(keyframe(&times, 0.5), (0, 0.5));
        assert_keyframe(keyframe(&times, 2.0), (1, 0.5));
        // The end of the range sits at the end of the last segment: the
        // returned index names the segment, not the keyframe.
        assert_keyframe(keyframe(&times, 3.0), (1, 1.0));
        assert_keyframe(keyframe(&times, 4.0), (1, 1.0));
    }

    #[test]
    fn a_step_holds_the_keyframe_before_the_time() {
        let times = [0.0, 1.0, 3.0];
        assert_eq!(step_frame(&times, -1.0), 0);
        assert_eq!(step_frame(&times, 0.0), 0);
        assert_eq!(step_frame(&times, 0.9), 0);
        assert_eq!(step_frame(&times, 1.0), 1);
        assert_eq!(step_frame(&times, 2.9), 1);
        assert_eq!(step_frame(&times, 3.0), 2);
        assert_eq!(step_frame(&times, 9.0), 2);
    }

    #[test]
    fn a_linear_translation_blends_between_its_keyframes() {
        let times = [0.0, 2.0];
        let values = [[0.0, 0.0, 0.0], [4.0, 2.0, 0.0]];
        let sample = |time| sample_vec3(Interpolation::Linear, &times, &values, time);
        assert_eq!(sample(1.0), [2.0, 1.0, 0.0]);
        // Outside the keyframe range the value clamps to the nearest end.
        assert_eq!(sample(-5.0), [0.0, 0.0, 0.0]);
        assert_eq!(sample(99.0), [4.0, 2.0, 0.0]);
    }

    #[test]
    fn a_step_translation_jumps_at_the_next_keyframe() {
        let times = [0.0, 1.0];
        let values = [[0.0, 0.0, 0.0], [5.0, 0.0, 0.0]];
        assert_eq!(
            sample_vec3(Interpolation::Step, &times, &values, 0.999),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            sample_vec3(Interpolation::Step, &times, &values, 1.0),
            [5.0, 0.0, 0.0]
        );
    }

    #[test]
    fn a_cubic_translation_follows_its_tangents() {
        // Two keyframes, each written as (in-tangent, value, out-tangent). The
        // first in-tangent and the last out-tangent never enter the formula.
        let times = [0.0, 1.0];
        let values = [
            [9.0, 9.0, 9.0],
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [9.0, 9.0, 9.0],
        ];
        let sample = |time| sample_vec3(Interpolation::CubicSpline, &times, &values, time);
        assert_eq!(sample(0.0), [0.0, 0.0, 0.0]);
        assert_eq!(sample(1.0), [4.0, 0.0, 0.0]);
        assert_eq!(sample(0.5), [1.875, 0.0, 0.0]);
    }

    #[test]
    fn a_linear_rotation_slerps_toward_its_keyframe() {
        let quarter = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let values = [
            [0.0, 0.0, 0.0, 1.0],
            [quarter.x, quarter.y, quarter.z, quarter.w],
        ];
        let half = sample_rotation(Interpolation::Linear, &[0.0, 1.0], &values, 0.5);
        let eighth = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_4);
        assert!(half.dot(eighth).abs() > 1.0 - 1e-6, "{half:?}");
    }

    #[test]
    fn a_linear_rotation_takes_the_short_way_around() {
        // +170° to -170° is a 20° step through 180°, not a 340° one.
        let a = glam::Quat::from_rotation_z(170f32.to_radians());
        let b = glam::Quat::from_rotation_z((-170f32).to_radians());
        let values = [[a.x, a.y, a.z, a.w], [b.x, b.y, b.z, b.w]];
        let mid = sample_rotation(Interpolation::Linear, &[0.0, 1.0], &values, 0.5);
        let expected = glam::Quat::from_rotation_z(180f32.to_radians());
        assert!(mid.dot(expected).abs() > 1.0 - 1e-5, "{mid:?}");
    }

    #[test]
    fn a_cubic_rotation_stays_a_unit_quaternion() {
        let times = [0.0, 1.0];
        let values = [
            [0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.5, 0.0],
            [0.0, 0.0, -0.5, 0.0],
            [
                0.0,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
            [0.0, 0.0, 0.0, 0.0],
        ];
        let value = sample_rotation(Interpolation::CubicSpline, &times, &values, 0.5);
        assert!((value.length() - 1.0).abs() < 1e-5, "{value:?}");
    }

    #[test]
    fn a_linear_weight_blend_is_per_target() {
        let times = [0.0, 2.0];
        // Two targets per keyframe: 0, 10 then 2, 20.
        let values = [0.0, 10.0, 2.0, 20.0];
        let mut out = vec![99.0; 5];
        sample_weights(Interpolation::Linear, &times, &values, 2, 1.0, &mut out);
        // The buffer is cleared, not just filled up to the target count.
        assert_eq!(out, vec![1.0, 15.0]);
    }

    #[test]
    fn a_cubic_weight_blend_reads_its_kind_first_layout() {
        // One target, two keyframes. A cubic keyframe groups every in-tangent,
        // then every value, then every out-tangent, so the second keyframe's
        // value sits after both keyframes' in-tangent and value blocks.
        let times = [0.0, 1.0];
        let values = [0.0, 1.0, 2.0, 3.0, 9.0, 4.0];
        let mut out = Vec::new();
        sample_weights(
            Interpolation::CubicSpline,
            &times,
            &values,
            1,
            0.5,
            &mut out,
        );
        assert_eq!(out, vec![4.875]);
    }
}

#[cfg(test)]
mod model_tests {
    use super::Interpolation as LitModelInterpolation;
    use super::*;

    fn channel(
        interpolation: LitModelInterpolation,
        times: Vec<f32>,
        values: LitModelOutput,
    ) -> LitModelChannel {
        LitModelChannel {
            node: 0,
            interpolation,
            times,
            values,
        }
    }

    #[test]
    fn step_sampling_holds_the_last_key() {
        let channel = channel(
            LitModelInterpolation::Step,
            vec![0.0, 1.0],
            LitModelOutput::Translation(vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]),
        );
        let mut local = Transform::default();
        let mut weights = Vec::new();
        sample_channel(&channel, 2.0, &mut local, &mut weights);
        assert_eq!(local.translation, glam::Vec3::new(4.0, 5.0, 6.0));
    }

    #[test]
    fn one_cubic_key_uses_the_value_slot() {
        let channel = channel(
            LitModelInterpolation::CubicSpline,
            vec![0.0],
            LitModelOutput::Translation(vec![[9.0, 9.0, 9.0], [1.0, 2.0, 3.0], [8.0, 8.0, 8.0]]),
        );
        let mut local = Transform::default();
        let mut weights = Vec::new();
        sample_channel(&channel, 0.0, &mut local, &mut weights);
        assert_eq!(local.translation, glam::Vec3::new(1.0, 2.0, 3.0));
    }
}
