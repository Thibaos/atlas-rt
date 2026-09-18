use glam::{IVec3, Mat4, Vec3, Vec4};

use crate::world::World;
use crate::world::grid::in_lattice;

#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub origin: Vec3,
    pub direction: Vec3,
    pub t_max: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct VoxelHit {
    pub voxel: IVec3,
    pub normal: IVec3,
    pub t: f32,
    pub material: u8,
}

impl Ray {
    #[must_use]
    pub fn new(origin: Vec3, direction: Vec3) -> Self {
        Self {
            origin,
            direction: direction.normalize_or_zero(),
            t_max: f32::INFINITY,
        }
    }

    #[must_use]
    pub const fn with_max_distance(self, t_max: f32) -> Self {
        Self { t_max, ..self }
    }
}

impl World {
    #[must_use]
    pub fn raycast(&self, ray: Ray) -> Option<VoxelHit> {
        raycast(self, ray)
    }
}

/// Builds the ray through the centre of the frame.
///
/// The matrices are the ones the renderer hands the GPU. The embedded path's
/// view carries `render::camera::mirror_right`, since the renderer's world is
/// left-handed and the ray generator maps view +x to screen right.
#[must_use]
pub fn screen_center_ray(proj_inverse: Mat4, view_inverse: Mat4) -> Ray {
    let origin = view_inverse.transform_point3(Vec3::ZERO);
    let eye = proj_inverse
        .mul_vec4(Vec4::new(0.0, 0.0, -1.0, 1.0))
        .truncate()
        .normalize_or_zero();
    let direction = view_inverse.transform_vector3(eye).normalize_or_zero();

    Ray {
        origin,
        direction,
        t_max: 10000.0,
    }
}

/// The global voxel coordinate a point falls in, matching the renderer's
/// half-open cells.
#[allow(clippy::cast_possible_truncation)]
const fn cell_of(point: Vec3) -> IVec3 {
    IVec3::new(
        point.x.floor() as i32,
        point.y.floor() as i32,
        point.z.floor() as i32,
    )
}

/// The normal of the face a ray entered the current cell through.
fn crossed_normal(axis: usize, direction: Vec3) -> IVec3 {
    let sign = |component: f32| if component > 0.0 { -1 } else { 1 };

    if axis == 0 {
        IVec3::new(sign(direction.x), 0, 0)
    } else if axis == 1 {
        IVec3::new(0, sign(direction.y), 0)
    } else {
        IVec3::new(0, 0, sign(direction.z))
    }
}

/// The normal of the cell the ray starts in, which it entered through no face.
fn starting_normal(direction: Vec3) -> IVec3 {
    if direction.x != 0.0 {
        crossed_normal(0, direction)
    } else if direction.y != 0.0 {
        crossed_normal(1, direction)
    } else {
        crossed_normal(2, direction)
    }
}

/// One axis's step, distance to its next boundary crossing, and the spacing
/// between crossings.
#[allow(clippy::cast_precision_loss)]
fn axis_state(origin: f32, direction: f32, cell: i32) -> (i32, f32, f32) {
    if direction == 0.0 {
        return (1, f32::INFINITY, f32::INFINITY);
    }

    if direction > 0.0 {
        let boundary = (cell + 1) as f32;

        (1, (boundary - origin) / direction, 1.0 / direction)
    } else {
        let boundary = cell as f32;

        (-1, (boundary - origin) / direction, -1.0 / direction)
    }
}

/// Moves every axis's crossing forward to the current cell's entry distance.
///
/// An axis not crossed on the way in carries the crossing that happened before
/// the current cell, which is behind the ray. Left alone it would send the walk
/// back across a boundary it has already passed. A direction of zero gives an
/// infinite crossing and an infinite spacing, so the loop exits at once.
fn advance_past(t_next: (f32, f32, f32), t_delta: (f32, f32, f32), entry: f32) -> (f32, f32, f32) {
    let advance = |t: f32, delta: f32| {
        let mut crossing = t;

        while crossing < entry {
            crossing += delta;
        }

        crossing
    };

    (
        advance(t_next.0, t_delta.0),
        advance(t_next.1, t_delta.1),
        advance(t_next.2, t_delta.2),
    )
}

#[must_use]
pub fn raycast(world: &World, ray: Ray) -> Option<VoxelHit> {
    let Ray {
        origin,
        direction,
        t_max,
    } = ray;

    let start = cell_of(origin);

    if !in_lattice(start) {
        return None;
    }

    let (step_x, t_next_x, delta_x) = axis_state(origin.x, direction.x, start.x);
    let (step_y, t_next_y, delta_y) = axis_state(origin.y, direction.y, start.y);
    let (step_z, t_next_z, delta_z) = axis_state(origin.z, direction.z, start.z);

    let mut current = start;
    let mut t_next = (t_next_x, t_next_y, t_next_z);
    let t_delta = (delta_x, delta_y, delta_z);
    let mut entered: Option<usize> = None;
    let mut entry_t = 0.0;

    loop {
        let material = world.get_voxel(&current).copied();

        if let Some(material) = material {
            let normal = entered.map_or_else(
                || starting_normal(direction),
                |axis| crossed_normal(axis, direction),
            );

            return Some(VoxelHit {
                voxel: current,
                normal,
                t: entry_t,
                material: u8::try_from(material).unwrap_or(0),
            });
        }

        let (x, y, z) = advance_past(t_next, t_delta, entry_t);
        let (axis, step, delta, crossing) = if x <= y && x <= z {
            (0, step_x, t_delta.0, x)
        } else if y <= z {
            (1, step_y, t_delta.1, y)
        } else {
            (2, step_z, t_delta.2, z)
        };

        if crossing >= t_max {
            return None;
        }

        let stepped = if axis == 0 {
            IVec3::new(current.x.wrapping_add(step), current.y, current.z)
        } else if axis == 1 {
            IVec3::new(current.x, current.y.wrapping_add(step), current.z)
        } else {
            IVec3::new(current.x, current.y, current.z.wrapping_add(step))
        };

        if !in_lattice(stepped) {
            return None;
        }

        let advanced = crossing + delta;

        t_next = if axis == 0 {
            (advanced, y, z)
        } else if axis == 1 {
            (x, advanced, z)
        } else {
            (x, y, advanced)
        };

        current = stepped;
        entered = Some(axis);
        entry_t = crossing;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::camera::camera_view;

    const PROJ_NEAR: f32 = 0.01;
    const PROJ_FAR: f32 = 10000.0;
    const FOV: f32 = std::f32::consts::FRAC_PI_2;
    const TOLERANCE: f32 = 1.0e-3;

    fn world_with(voxels: &[(IVec3, u32)]) -> World {
        let mut world = World::default();

        for (position, material) in voxels {
            world.insert_voxel_at(*position, *material);
        }

        world
    }

    fn near(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < TOLERANCE
    }

    /// The point a hit reports lies on the face its normal names, inside the
    /// voxel's other two axes.
    fn landed_on_the_named_face(hit: &VoxelHit, ray: &Ray) {
        let landed = ray.origin + ray.direction * hit.t;
        let inside = landed - hit.voxel.as_vec3();
        let [x, y, z] = [inside.x, inside.y, inside.z];
        let x_edge = near(x, 0.0) || near(x, 1.0);
        let y_edge = near(y, 0.0) || near(y, 1.0);
        let z_edge = near(z, 0.0) || near(z, 1.0);
        let reached = if hit.normal.x != 0 {
            x_edge
        } else if hit.normal.y != 0 {
            y_edge
        } else {
            z_edge
        };
        let within = [x, y, z]
            .into_iter()
            .all(|axis| axis > -TOLERANCE && axis < 1.0 + TOLERANCE);

        assert!(
            reached && within,
            "the hit point {landed:?} is not on the {:?} face of {:?}",
            hit.normal,
            hit.voxel
        );
    }

    fn hit(world: &World, ray: Ray) -> VoxelHit {
        let origin = ray.origin;

        world
            .raycast(ray)
            .unwrap_or_else(|| panic!("expected a hit from {origin:?}"))
    }

    #[test]
    fn a_ray_down_an_axis_hits_the_voxel_on_it() {
        let world = world_with(&[(IVec3::ZERO, 7)]);
        let result = hit(&world, Ray::new(Vec3::new(-5.0, 0.5, 0.5), Vec3::X));

        assert_eq!(result.voxel, IVec3::ZERO);
        assert_eq!(result.normal, IVec3::new(-1, 0, 0));
        assert!(near(result.t, 5.0));
        assert_eq!(result.material, 7);
    }

    #[test]
    fn a_ray_that_misses_reports_nothing() {
        let world = world_with(&[(IVec3::ZERO, 7)]);

        assert!(
            world
                .raycast(Ray::new(Vec3::new(-5.0, 4.5, 0.5), Vec3::X))
                .is_none()
        );
        assert!(
            World::default()
                .raycast(Ray::new(Vec3::ZERO, Vec3::X))
                .is_none()
        );
    }

    #[test]
    fn a_wall_reports_its_first_voxel() {
        let world = world_with(&[(IVec3::ZERO, 3), (IVec3::X, 4)]);
        let result = hit(&world, Ray::new(Vec3::new(-5.0, 0.5, 0.5), Vec3::X));

        assert_eq!(result.voxel, IVec3::ZERO);
        assert_eq!(result.material, 3);
    }

    #[test]
    fn a_ray_from_inside_the_only_voxel_hits_it() {
        let world = world_with(&[(IVec3::ZERO, 7)]);

        assert!(
            world
                .raycast(Ray::new(Vec3::new(0.5, 0.5, 0.5), Vec3::X))
                .is_some()
        );
    }

    #[test]
    fn an_origin_on_a_cell_boundary_belongs_to_the_cell_it_heads_into() {
        let world = world_with(&[(IVec3::new(0, 400, 500), 9)]);
        let forward = hit(&world, Ray::new(Vec3::new(0.0, 400.5, 500.5), Vec3::X));

        assert_eq!(forward.voxel, IVec3::new(0, 400, 500));
        assert_eq!(forward.normal, IVec3::new(-1, 0, 0));
        assert!(near(forward.t, 0.0));

        let backward = hit(&world, Ray::new(Vec3::new(0.0, 400.5, 500.5), Vec3::NEG_X));

        assert_eq!(backward.voxel, IVec3::new(0, 400, 500));
        assert_eq!(backward.normal, IVec3::new(1, 0, 0));
        assert!(near(backward.t, 0.0));
    }

    #[test]
    fn an_origin_inside_a_voxel_hits_it_at_zero() {
        let world = world_with(&[(IVec3::ZERO, 11)]);
        let result = hit(&world, Ray::new(Vec3::new(0.5, 0.5, 0.5), Vec3::X));

        assert_eq!(result.voxel, IVec3::ZERO);
        assert_eq!(result.normal, IVec3::new(-1, 0, 0));
        assert!(near(result.t, 0.0));
    }

    #[test]
    fn a_ray_parallel_to_a_face_hits_the_cell_it_starts_on() {
        let world = world_with(&[(IVec3::ZERO, 5)]);
        let result = hit(&world, Ray::new(Vec3::new(0.5, 0.5, 0.5), Vec3::Y));

        assert_eq!(result.voxel, IVec3::ZERO);
        assert_eq!(result.normal, IVec3::new(0, -1, 0));
        assert!(near(result.t, 0.0));
    }

    #[test]
    fn a_ray_short_of_the_voxel_reports_nothing() {
        let world = world_with(&[(IVec3::ZERO, 7)]);
        let ray = Ray::new(Vec3::new(-5.0, 0.5, 0.5), Vec3::X).with_max_distance(4.0);

        assert!(world.raycast(ray).is_none());
    }

    #[test]
    fn an_origin_outside_the_lattice_reports_nothing() {
        let world = world_with(&[(IVec3::ZERO, 7)]);
        let far = Vec3::splat(5000.0);

        assert!(world.raycast(Ray::new(far, Vec3::X)).is_none());
    }

    #[test]
    fn the_constructor_normalizes_the_direction() {
        let ray = Ray::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 12.0));

        assert!(near(ray.direction.length(), 1.0));
        assert!(near(ray.direction.z, 1.0));
    }

    #[test]
    fn a_shortest_path_crossing_two_axes_enters_through_the_later_face() {
        let world = world_with(&[(IVec3::ZERO, 3)]);
        let origin = Vec3::new(-1.5, 1.5, 0.5);
        let ray = Ray::new(origin, Vec3::new(1.0, -1.0, 0.0));
        let result = hit(&world, ray);

        assert_eq!(result.voxel, IVec3::ZERO);
        assert_eq!(result.normal, IVec3::new(-1, 0, 0));
        assert!(near(result.t, 1.5 * 2.0_f32.sqrt()));
        landed_on_the_named_face(&result, &ray);
    }

    #[test]
    fn a_hit_lands_on_the_face_its_normal_names() {
        let world = world_with(&[(IVec3::ZERO, 7)]);
        let origin = Vec3::new(-5.0, 0.5, 0.5);
        let ray = Ray::new(origin, Vec3::X);
        let result = hit(&world, ray);

        assert_eq!(result.normal, IVec3::new(-1, 0, 0));
        assert!(
            result.normal.as_vec3().dot(ray.direction) < 0.0,
            "the normal {:?} does not face the ray {ray:?}",
            result.normal
        );

        landed_on_the_named_face(&result, &ray);
    }

    #[test]
    fn a_material_survives_as_a_palette_index() {
        let world = world_with(&[(IVec3::new(0, 400, 500), 200)]);
        let ray = Ray::new(Vec3::new(5.0, 400.5, 500.5), Vec3::NEG_X);

        assert_eq!(u32::from(hit(&world, ray).material), 200);
    }

    #[test]
    fn the_centre_ray_follows_the_cameras_forward_axis() {
        let eye = Vec3::new(31.0, 300.0, 500.0);
        let axes = [Vec3::X, Vec3::Y, Vec3::Z];
        let view = camera_view(eye, axes);
        let proj =
            glam::camera::lh::proj::vulkan::perspective(FOV, 16.0 / 9.0, PROJ_NEAR, PROJ_FAR);
        let ray = screen_center_ray(proj.inverse(), view.inverse());

        assert!(near(ray.origin.distance(eye), 0.0));
        assert!(near(ray.direction.distance(Vec3::Z), 0.0));
        assert!(near(ray.t_max, PROJ_FAR));
    }
}
