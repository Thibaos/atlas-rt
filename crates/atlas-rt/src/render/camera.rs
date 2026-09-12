use glam::{Mat4, Vec3, Vec4};

/// The view matrix the ray generator expects: the camera's right on view x, its
/// up on view y, its forward on view z.
///
/// `axes` are the camera's own right, up and forward in world space, so its
/// `forward` is the direction the camera looks along. Each lands in the row
/// carrying the view axis it names, which is what makes the matrix a
/// world-to-view transform.
#[must_use]
pub fn camera_view(origin: Vec3, axes: [Vec3; 3]) -> Mat4 {
    let [right, up, forward] = axes;

    Mat4::from_cols(
        Vec4::new(right.x, up.x, forward.x, 0.0),
        Vec4::new(right.y, up.y, forward.y, 0.0),
        Vec4::new(right.z, up.z, forward.z, 0.0),
        Vec4::new(
            -right.dot(origin),
            -up.dot(origin),
            -forward.dot(origin),
            1.0,
        ),
    )
}

/// The world mirror a host camera needs to draw the scene the way the standalone
/// app draws it.
///
/// The renderer holds the world left handed and the ray generator turns view +x
/// into screen right, while a host hands over a right handed camera basis. Fed
/// in unchanged, a host draws the world's +x on the other side of the frame and
/// the scene reads mirrored. Mirroring the camera's right axis onto the world
/// axis the world is already mirrored on cancels it.
#[must_use]
#[allow(clippy::arithmetic_side_effects)]
pub fn mirror_right(axes: [Vec3; 3]) -> [Vec3; 3] {
    let [right, up, forward] = axes;

    [-right, up, forward]
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)] // the tests compare floating point
mod tests {
    use super::*;

    /// World units. Two constructions of the same view agree to this much, which
    /// f32 costs at the eye distances this renderer works at.
    const EPSILON: f32 = 1.0e-2;

    fn near(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < EPSILON
    }

    fn near_vec(actual: Vec3, expected: Vec3) -> bool {
        (actual - expected).abs().max_element() < EPSILON
    }

    fn columns(matrix: Mat4) -> [[f32; 4]; 4] {
        matrix.to_cols_array_2d()
    }

    /// A point on the camera's right reads positive, one on its left negative.
    /// View x is that axis, because the ray generator turns it into screen right.
    fn screen_x(view: Mat4, point: Vec3) -> f32 {
        view.transform_point3(point).x
    }

    /// The direction the camera looks along and its up, for a yaw and a pitch.
    fn pose(yaw: f32, pitch: f32) -> (Vec3, Vec3) {
        let rotation = glam::Quat::from_rotation_y(yaw) * glam::Quat::from_rotation_x(pitch);

        (rotation.mul_vec3(Vec3::NEG_Z), rotation.mul_vec3(Vec3::Y))
    }

    /// A host camera basis as a host hands one over: the camera's own axes,
    /// right handed because a host's are, so its right is `forward x up`.
    fn host_basis(yaw: f32, pitch: f32) -> [Vec3; 3] {
        let (forward, up) = pose(yaw, pitch);

        [forward.cross(up), up, forward]
    }

    /// The camera axes a view matrix carries.
    fn view_axes(view: Mat4) -> [Vec3; 3] {
        let [row_0, row_1, row_2, _] = view.to_cols_array_2d();

        [
            Vec3::new(row_0[0], row_1[0], row_2[0]),
            Vec3::new(row_0[1], row_1[1], row_2[1]),
            Vec3::new(row_0[2], row_1[2], row_2[2]),
        ]
    }

    /// The view the standalone app builds for the same pose.
    fn standalone_view(eye: Vec3, yaw: f32, pitch: f32) -> Mat4 {
        let (forward, up) = pose(yaw, pitch);
        let basis = glam::camera::lh::view::look_at_mat4(eye, eye + forward, up);

        camera_view(eye, view_axes(basis))
    }

    #[test]
    fn a_view_keeps_the_camera_pose() {
        let eye = Vec3::new(3.0, 8.0, 40.0);
        let view = camera_view(eye, host_basis(0.7, -0.2));

        assert!(near_vec(view.inverse().w_axis.truncate(), eye));
    }

    #[test]
    fn a_point_on_the_camera_right_reads_to_the_right_of_the_frame() {
        let eye = Vec3::new(0.0, 300.0, 500.0);
        let axes = host_basis(0.0, 0.0);
        let [right, _, forward] = axes;
        let view = camera_view(eye, axes);
        let ahead = eye + forward * 100.0;

        assert!(near(screen_x(view, ahead + right * 100.0), 100.0));
        assert!(near(screen_x(view, ahead - right * 100.0), -100.0));
    }

    #[test]
    fn the_standalone_camera_reads_the_world_the_other_way_round() {
        let [right, up, forward] = host_basis(0.4, -0.3);
        let [standalone_right, _, _] = view_axes(standalone_view(Vec3::ZERO, 0.4, -0.3));

        assert!(near(right.dot(up.cross(forward)), -1.0));
        assert!(
            near_vec(standalone_right, -right),
            "the standalone right {standalone_right:?} is not the host right {right:?} negated"
        );
    }

    #[test]
    fn the_mirrored_basis_draws_what_the_standalone_camera_draws() {
        for yaw in [-1.2_f32, -0.4, 0.0, 0.35, 2.9] {
            for pitch in [-0.6_f32, 0.0, 0.45] {
                let eye = Vec3::new(31.0, 300.0, 500.0);
                let mirrored = camera_view(eye, mirror_right(host_basis(yaw, pitch)));
                let standalone = standalone_view(eye, yaw, pitch);

                for (column, other) in columns(mirrored).into_iter().zip(columns(standalone)) {
                    for (value, expected) in column.into_iter().zip(other) {
                        assert!(
                            near(value, expected),
                            "yaw {yaw} pitch {pitch}: {mirrored:?} differs from {standalone:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn mirroring_moves_a_world_point_to_the_other_side_of_the_frame() {
        let eye = Vec3::new(0.0, 300.0, 500.0);
        let point = Vec3::new(100.0, 300.0, 400.0);
        let axes = host_basis(0.0, 0.0);

        let direct = screen_x(camera_view(eye, axes), point);
        let mirrored = screen_x(camera_view(eye, mirror_right(axes)), point);

        assert!(
            direct * mirrored < 0.0,
            "both bases put the point on the same side: {direct} and {mirrored}"
        );
        assert!(near(mirrored, -direct));
    }
}
