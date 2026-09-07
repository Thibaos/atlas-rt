use dot_vox::{DotVoxData, Rotation, SceneNode, Voxel};
use glam::{IVec3, UVec3};

#[cfg(test)]
use glam::{Mat4, Vec3A, Vec3Swizzles};

use crate::world::{InsertResult, grid};

use super::{BoundsPolicy, World};

pub struct SceneGraphTraverser<'world, 'scene> {
    pub world: &'world mut World,
    pub policy: BoundsPolicy,
    pub scene: &'scene DotVoxData,
    pub models: Vec<(IVec3, Rotation, UVec3, &'scene [Voxel])>,
}

impl SceneGraphTraverser<'_, '_> {
    pub fn traverse(&mut self) -> usize {
        if self.scene.scenes.is_empty() {
            let mut clipped = 0usize;
            for voxel in self.scene.models.iter().flat_map(|model| &model.voxels) {
                if self.world.insert(
                    IVec3::new(i32::from(voxel.x), i32::from(voxel.z), i32::from(voxel.y)),
                    u32::from(voxel.i),
                    self.policy,
                ) == InsertResult::Clipped
                {
                    clipped = clipped.saturating_add(1);
                }
            }
            clipped
        } else {
            self.traverse_recursive(0, IVec3::ZERO, Rotation::IDENTITY);
            0
        }
    }

    fn traverse_recursive(&mut self, node: u32, translation: IVec3, rotation: Rotation) {
        let Some(node) = self
            .scene
            .scenes
            .get(usize::try_from(node).unwrap_or(usize::MAX))
        else {
            panic!("scene node {node} out of range");
        };

        match node {
            SceneNode::Transform { frames, child, .. } => {
                let [frame] = &frames[..] else {
                    panic!(
                        "transform node must have exactly one frame, got {}",
                        frames.len()
                    );
                };

                let this_translation = frame.position().map_or(IVec3::ZERO, |position| IVec3 {
                    x: position.x,
                    y: position.y,
                    z: position.z,
                });

                let this_rotation = frame.orientation().unwrap_or(Rotation::IDENTITY);

                let translation = translation.saturating_add(this_translation);

                self.traverse_recursive(*child, translation, compose(rotation, this_rotation));
            }
            SceneNode::Group { children, .. } => {
                for child in children {
                    self.traverse_recursive(*child, translation, rotation);
                }
            }
            SceneNode::Shape { models, .. } => {
                let [shape_model] = models.as_slice() else {
                    panic!(
                        "shape node must have exactly one model, got {}",
                        models.len()
                    );
                };

                let scene = self.scene;
                let model = scene
                    .models
                    .get(usize::try_from(shape_model.model_id).unwrap_or(usize::MAX))
                    .unwrap_or_else(|| panic!("shape model {} out of range", shape_model.model_id));

                if model.voxels.is_empty() {
                    return;
                }

                let size = model.size;

                self.models.push((
                    translation,
                    rotation,
                    UVec3::new(size.x, size.y, size.z),
                    model.voxels.as_slice(),
                ));
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn legacy_float_transform(
        translation: IVec3,
        rotation: Rotation,
        size: UVec3,
    ) -> Mat4 {
        use std::ops::{Add, Mul, Sub};

        use glam::Quat;

        let mut translation = translation.as_vec3a().xzy();
        translation.z *= -1.0;

        let (quat, scale) = rotation.to_quat_scale();
        let quat = Quat::from_array(quat);
        let quat = Quat::from_xyzw(quat.x, quat.z, -quat.y, quat.w);
        let scale = Vec3A::from_array(scale).xzy();

        let mut offset = Vec3A::new(
            if size.x.is_multiple_of(2) { 0.0 } else { 0.5 },
            if size.z.is_multiple_of(2) { 0.0 } else { 0.5 },
            if size.y.is_multiple_of(2) { 0.0 } else { -0.5 },
        );

        offset = quat.mul_vec3a(offset);

        let center = quat.mul_vec3a(size.xzy().as_vec3a().mul(0.5));

        Mat4::from_scale_rotation_translation(
            scale.into(),
            quat,
            translation.sub(center.mul(scale)).add(offset).into(),
        )
    }
}

pub struct VoxelPlacement {
    rows: [IVec3; 3],
    translation: IVec3,
    center: IVec3,
    size: UVec3,
}

impl VoxelPlacement {
    pub fn new(translation: IVec3, rotation: Rotation, size: UVec3) -> Self {
        let columns = rotation.to_cols_array_2d();
        let one = 1.0_f32.to_bits();
        let minus_one = (-1.0_f32).to_bits();

        let unit_row = |row: &[f32; 3]| -> IVec3 {
            let mut picked: Option<(usize, i32)> = None;
            for (column, value) in row.iter().enumerate() {
                let sign = match value.to_bits() {
                    bits if bits == one => 1,
                    bits if bits == minus_one => -1,
                    _ => 0,
                };

                if sign == 0 {
                    continue;
                }

                assert!(
                    picked.is_none(),
                    "rotation row {row:?} is not a signed permutation"
                );

                picked = Some((column, sign));
            }

            match picked
                .unwrap_or_else(|| panic!("rotation row {row:?} is not a signed permutation"))
            {
                (0, sign) => IVec3::new(sign, 0, 0),
                (1, sign) => IVec3::new(0, sign, 0),
                (_, sign) => IVec3::new(0, 0, sign),
            }
        };

        let model_rows = [
            unit_row(&[columns[0][0], columns[1][0], columns[2][0]]),
            unit_row(&[columns[0][1], columns[1][1], columns[2][1]]),
            unit_row(&[columns[0][2], columns[1][2], columns[2][2]]),
        ];

        let determinant = model_rows[0].dot(model_rows[1].cross(model_rows[2]));

        Self {
            rows: [model_rows[0], model_rows[2], model_rows[1]],
            translation: IVec3::new(translation.x, translation.z, translation.y),
            center: IVec3::new(
                half_center(size.x, determinant),
                half_center(size.y, determinant),
                half_center(size.z, determinant),
            ),
            size,
        }
    }

    pub fn misses_lattice(&self) -> bool {
        let half = grid::LATTICE_HALF_EXTENT.cast_signed();
        let neg_half = half.wrapping_neg();

        let corner = |extent: u32| {
            u8::try_from(extent.min(256).saturating_sub(1))
                .unwrap_or_else(|_| panic!("model extent {extent} out of voxel range"))
        };
        let low = self.project(0, 0, 0);
        let high = self.project(
            i32::from(corner(self.size.x)),
            i32::from(corner(self.size.y)),
            i32::from(corner(self.size.z)),
        );
        let min = low.min(high);
        let max = low.max(high);

        min.x >= half
            || min.y >= half
            || min.z >= half
            || max.x < neg_half
            || max.y < neg_half
            || max.z < neg_half
    }

    pub fn in_lattice_capacity(&self, voxels: u64) -> u64 {
        let corner = |extent: u32| i32::try_from(extent.saturating_sub(1)).unwrap_or(i32::MAX);
        let low = self.project(0, 0, 0);
        let high = self.project(
            corner(self.size.x),
            corner(self.size.y),
            corner(self.size.z),
        );
        let min = low.min(high);
        let max = low.max(high);

        let half = grid::LATTICE_HALF_EXTENT.cast_signed();
        let neg_half = half.wrapping_neg();
        let high_edge = half.saturating_sub(1);

        let span = |lo: i32, hi: i32| -> u64 {
            let lo = lo.max(neg_half);
            let hi = hi.min(high_edge);

            if hi < lo {
                return 0;
            }

            let length = i64::from(hi).saturating_sub(i64::from(lo)).saturating_add(1);
            u64::try_from(length).unwrap_or(u64::MAX)
        };

        span(min.x, max.x)
            .saturating_mul(span(min.y, max.y))
            .saturating_mul(span(min.z, max.z))
            .min(voxels)
    }

    pub fn place(&self, voxel: Voxel) -> IVec3 {
        self.project(i32::from(voxel.x), i32::from(voxel.y), i32::from(voxel.z))
    }

    fn project(&self, x: i32, y: i32, z: i32) -> IVec3 {
        let shifted = IVec3::new(
            x.wrapping_sub(self.center.x),
            y.wrapping_add(1).wrapping_sub(self.center.y),
            z.wrapping_sub(self.center.z),
        );

        IVec3::new(
            self.translation.x.saturating_add(self.rows[0].dot(shifted)),
            self.translation.y.saturating_add(self.rows[1].dot(shifted)),
            self.translation.z.saturating_add(self.rows[2].dot(shifted)),
        )
    }
}

fn half_center(size: u32, determinant: i32) -> i32 {
    let half = if determinant < 0 {
        size.div_euclid(2).saturating_add(size & 1)
    } else {
        size.div_euclid(2)
    };

    i32::try_from(half)
        .unwrap_or_else(|_| panic!("model size {size} too large for integer placement"))
}

#[allow(clippy::arithmetic_side_effects)] // dot_vox composes packed rotation bitfields
fn compose(rotation: Rotation, next: Rotation) -> Rotation {
    rotation * next
}

#[cfg(test)]
mod tests {
    use dot_vox::{Rotation, Voxel};
    use glam::{IVec3, UVec3};

    use super::VoxelPlacement;
    use crate::world::grid;

    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }

        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    fn rotation_bytes() -> Vec<u8> {
        (0u8..128)
            .filter(|byte| {
                let first = byte & 0b11;
                let second = (byte >> 2) & 0b11;
                first != 0b11 && second != 0b11 && first != second
            })
            .collect()
    }

    fn placement(translation: [i32; 3], rotation: u8, size: (u32, u32, u32)) -> VoxelPlacement {
        VoxelPlacement::new(
            IVec3::new(translation[0], translation[1], translation[2]),
            Rotation::from_byte(rotation),
            UVec3::new(size.0, size.1, size.2),
        )
    }

    fn corner_aabb(placement: &VoxelPlacement) -> (IVec3, IVec3) {
        let corner = |extent: u32| i32::try_from(extent - 1).unwrap_or(i32::MAX);
        let low = placement.project(0, 0, 0);
        let high = placement.project(
            corner(placement.size.x),
            corner(placement.size.y),
            corner(placement.size.z),
        );

        (low.min(high), low.max(high))
    }

    fn brute_force_capacity(placement: &VoxelPlacement) -> u64 {
        let (min, max) = corner_aabb(placement);
        let half = grid::LATTICE_HALF_EXTENT.cast_signed();
        let neg_half = half.wrapping_neg();

        let span = |lo: i32, hi: i32| -> u64 {
            let lo = lo.max(neg_half);
            let hi = hi.min(half - 1);

            if hi < lo {
                return 0;
            }

            u64::try_from(hi - lo + 1).unwrap_or(u64::MAX)
        };

        span(min.x, max.x) * span(min.y, max.y) * span(min.z, max.z)
    }

    fn dense_in_lattice(translation: [i32; 3], rotation: u8, size: (u32, u32, u32)) -> u64 {
        let placement = placement(translation, rotation, size);
        let mut count = 0u64;

        for x in 0..size.0 {
            for y in 0..size.1 {
                for z in 0..size.2 {
                    let voxel = Voxel {
                        x: x as u8,
                        y: y as u8,
                        z: z as u8,
                        i: 0,
                    };

                    if grid::in_lattice(placement.place(voxel)) {
                        count += 1;
                    }
                }
            }
        }

        count
    }

    fn sample_distinct_voxels(rng: &mut Rng, size: (u32, u32, u32), count: u64) -> Vec<Voxel> {
        let volume = u64::from(size.0) * u64::from(size.1) * u64::from(size.2);
        let mut cells: Vec<u32> = (0..volume as u32).collect();

        for index in 0..count as usize {
            let swap = index + rng.below(volume - index as u64) as usize;
            cells.swap(index, swap);
        }

        cells[..count as usize]
            .iter()
            .map(|&cell| {
                let x = cell % size.0;
                let y = (cell / size.0) % size.1;
                let z = cell / (size.0 * size.1);
                Voxel {
                    x: x as u8,
                    y: y as u8,
                    z: z as u8,
                    i: 0,
                }
            })
            .collect()
    }

    #[test]
    fn known_legacy_example_reserves_one_slot() {
        let placement = placement([5, 7, 9], 0b0001, (1, 1, 1));

        assert_eq!(
            placement.place(Voxel {
                x: 0,
                y: 0,
                z: 0,
                i: 0
            }),
            IVec3::new(5, 8, 6)
        );
        assert_eq!(placement.in_lattice_capacity(1), 1);
    }

    #[test]
    fn capacity_matches_dense_ground_truth_for_all_rotations() {
        let size = (3u32, 2u32, 4u32);
        let attempts = 24u64;
        let translations = [
            [0, 0, 0],
            [10, -10, 7],
            [2044, -2044, 0],
            [2047, -2048, 1],
            [-3000, 3000, 5],
            [100_000, -100_000, 0],
        ];

        for rotation in rotation_bytes() {
            for translation in translations {
                let placement = placement(translation, rotation, size);
                let expected = dense_in_lattice(translation, rotation, size);

                assert_eq!(
                    placement.in_lattice_capacity(attempts),
                    expected,
                    "rotation {rotation:#010b} at {translation:?}"
                );
                assert_eq!(
                    placement.in_lattice_capacity(u64::MAX),
                    brute_force_capacity(&placement),
                    "rotation {rotation:#010b} at {translation:?}"
                );
            }
        }
    }

    #[test]
    fn fully_inside_models_reserve_the_full_attempt_count() {
        for size in [(1u32, 1u32, 1u32), (2, 2, 2), (3, 5, 7), (8, 8, 8), (255, 1, 2)] {
            let volume = u64::from(size.0) * u64::from(size.1) * u64::from(size.2);

            for rotation in [0b0000100u8, 0b0001] {
                for translation in [[0, 0, 0], [10, -20, 30], [1500, -1500, 1500]] {
                    let placement = placement(translation, rotation, size);

                    assert_eq!(
                        placement.in_lattice_capacity(volume),
                        volume,
                        "size {size:?}, rotation {rotation:#010b}, at {translation:?}"
                    );
                    assert_eq!(
                        placement.in_lattice_capacity(volume + 7),
                        volume,
                        "the box never exceeds its own volume"
                    );
                }
            }
        }
    }

    #[test]
    fn straddling_translations_clip_per_axis() {
        // identity on a 4x4x4 model: the x span is [t - 2, t + 1]
        for (translation, expected) in [
            ([2046, 0, 0], 64u64),
            ([2047, 0, 0], 48),
            ([2048, 0, 0], 32),
            ([2049, 0, 0], 16),
            ([2050, 0, 0], 0),
            ([-2046, 0, 0], 64),
            ([-2047, 0, 0], 48),
            ([-2048, 0, 0], 32),
            ([-2049, 0, 0], 16),
            ([-2050, 0, 0], 0),
        ] {
            let placement = placement(translation, 0b0000100, (4, 4, 4));

            assert_eq!(placement.in_lattice_capacity(64), expected, "{translation:?}");

            assert_eq!(placement.misses_lattice(), expected == 0, "{translation:?}");
        }
    }

    #[test]
    fn the_y_plus_one_shift_is_respected_on_straddle() {
        // identity on a (2, 4, 4) model: out_y = t_y + z - 2 and out_z = t_y + y - 1;
        // the input y translation drives out_z over the +1-shifted y input [-1, 2]
        let high = placement([0, 2047, 0], 0b0000100, (2, 4, 4));

        assert_eq!(high.in_lattice_capacity(32), 16);

        let low = placement([0, -2047, 0], 0b0000100, (2, 4, 4));

        assert_eq!(low.in_lattice_capacity(32), 32);
    }

    #[test]
    fn sparse_models_cap_the_capacity_at_the_attempt_count() {
        let placement = placement([0, 0, 0], 0b0000100, (5, 5, 5));

        assert_eq!(placement.in_lattice_capacity(10), 10);
        assert_eq!(placement.in_lattice_capacity(124), 124);
        assert_eq!(placement.in_lattice_capacity(125), 125);
        assert_eq!(placement.in_lattice_capacity(126), 125);
        assert_eq!(placement.in_lattice_capacity(u64::MAX), 125);
    }

    #[test]
    fn random_placements_bound_the_landed_attempts() {
        let mut rng = Rng::new(0x5EED_0707);
        let rotations = rotation_bytes();

        for _ in 0..128 {
            let size = (
                1 + rng.below(32) as u32,
                1 + rng.below(32) as u32,
                1 + rng.below(32) as u32,
            );
            let volume = u64::from(size.0) * u64::from(size.1) * u64::from(size.2);
            let attempts = 1 + rng.below(volume);
            let rotation = rotations[rng.below(rotations.len() as u64) as usize];
            let translation = [
                (rng.below(8_000) as i64 - 4_000) as i32,
                (rng.below(8_000) as i64 - 4_000) as i32,
                (rng.below(8_000) as i64 - 4_000) as i32,
            ];
            let placement = placement(translation, rotation, size);
            let voxels = sample_distinct_voxels(&mut rng, size, attempts);
            let landed = voxels
                .iter()
                .filter(|voxel| grid::in_lattice(placement.place(**voxel)))
                .count() as u64;

            let capacity = placement.in_lattice_capacity(attempts);

            assert!(capacity <= attempts, "rotation {rotation:#010b}, {translation:?}");
            assert!(
                landed <= capacity,
                "the reserve must cover every landed attempt: {landed} > {capacity}"
            );
            assert_eq!(
                capacity,
                brute_force_capacity(&placement).min(attempts),
                "rotation {rotation:#010b}, {translation:?}"
            );
            assert_eq!(
                placement.in_lattice_capacity(volume),
                dense_in_lattice(translation, rotation, size),
                "the dense box count must match the cell-by-cell ground truth"
            );

            let (min, max) = corner_aabb(&placement);
            let half = grid::LATTICE_HALF_EXTENT.cast_signed();
            let inside = |lo: i32, hi: i32| lo >= -half && hi < half;

            if inside(min.x, max.x) && inside(min.y, max.y) && inside(min.z, max.z) {
                assert_eq!(capacity, attempts, "fully inside: {translation:?}");
            }
        }
    }
}
