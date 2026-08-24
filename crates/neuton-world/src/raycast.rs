//! Finding the block a player is looking at.
//!
//! A voxel walk rather than fixed steps along the ray: stepping samples misses
//! a block the ray only clips the corner of, and picking a step small enough
//! not to means doing far more work than the walk. This visits exactly the
//! blocks the ray passes through, in order.

use crate::physics::{Aabb, BlockShapes, BlockView};
use neuton_blocks::StateId;

/// What the player is pointing at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    /// The block itself.
    pub block: [i32; 3],
    /// Which of its faces the ray entered by, in the game's order: down, up,
    /// north, south, west, east.
    pub face: u8,
    /// Where on the block the ray landed, from zero to one across each axis.
    /// Servers use it to decide which half of a slab a placement means.
    pub cursor: [f32; 3],
    /// The point in world space, for drawing.
    pub point: [f64; 3],
}

impl Hit {
    /// The block a placement against this face would go into.
    pub fn placement(&self) -> [i32; 3] {
        let [x, y, z] = self.block;
        match self.face {
            0 => [x, y - 1, z],
            1 => [x, y + 1, z],
            2 => [x, y, z - 1],
            3 => [x, y, z + 1],
            4 => [x - 1, y, z],
            _ => [x + 1, y, z],
        }
    }
}

/// How far a player can reach. The game's own figure for building.
pub const REACH: f64 = 4.5;

/// Walks `direction` from `origin` until it meets a block with any shape.
///
/// `direction` must be a unit vector.
pub fn cast(
    origin: [f64; 3],
    direction: [f64; 3],
    distance: f64,
    world: &dyn BlockView,
    shapes: &dyn BlockShapes,
) -> Option<Hit> {
    let mut block = [
        origin[0].floor() as i32,
        origin[1].floor() as i32,
        origin[2].floor() as i32,
    ];
    // Which way each axis steps, and how far along the ray one whole block of
    // that axis takes.
    let mut step = [0i32; 3];
    let mut next = [f64::INFINITY; 3];
    let mut delta = [f64::INFINITY; 3];
    for axis in 0..3 {
        if direction[axis] > 0.0 {
            step[axis] = 1;
            delta[axis] = 1.0 / direction[axis];
            next[axis] = (f64::from(block[axis] + 1) - origin[axis]) / direction[axis];
        } else if direction[axis] < 0.0 {
            step[axis] = -1;
            delta[axis] = -1.0 / direction[axis];
            next[axis] = (origin[axis] - f64::from(block[axis])) / -direction[axis];
        }
    }

    // The face of the first block is only known once the ray has crossed into
    // it, so the starting block is tested with no face and skipped if it is the
    // one the player is standing inside.
    let mut entered: Option<usize> = None;

    for _ in 0..512 {
        let state = world.state_at(block[0], block[1], block[2]);
        if let Some(hit) = meets(origin, direction, block, state, shapes, distance) {
            return Some(hit);
        }
        let _ = entered;

        // Step along whichever axis has the nearest boundary.
        let axis = if next[0] < next[1] && next[0] < next[2] {
            0
        } else if next[1] < next[2] {
            1
        } else {
            2
        };
        if next[axis] > distance {
            return None;
        }
        block[axis] += step[axis];
        next[axis] += delta[axis];
        entered = Some(axis);
    }
    None
}

/// Tests the ray against one block's shapes, nearest box first.
fn meets(
    origin: [f64; 3],
    direction: [f64; 3],
    block: [i32; 3],
    state: StateId,
    shapes: &dyn BlockShapes,
    distance: f64,
) -> Option<Hit> {
    let boxes = shapes.collision(state);
    if boxes.is_empty() {
        return None;
    }
    let base = [f64::from(block[0]), f64::from(block[1]), f64::from(block[2])];
    let mut best: Option<(f64, u8)> = None;
    for shape in boxes {
        let shape = Aabb::new(
            [base[0] + shape.min[0], base[1] + shape.min[1], base[2] + shape.min[2]],
            [base[0] + shape.max[0], base[1] + shape.max[1], base[2] + shape.max[2]],
        );
        if let Some((t, face)) = enters(origin, direction, &shape)
            && t <= distance
            && best.is_none_or(|(had, _)| t < had)
        {
            best = Some((t, face));
        }
    }

    let (t, face) = best?;
    let point = [
        origin[0] + direction[0] * t,
        origin[1] + direction[1] * t,
        origin[2] + direction[2] * t,
    ];
    Some(Hit {
        block,
        face,
        cursor: [
            (point[0] - base[0]) as f32,
            (point[1] - base[1]) as f32,
            (point[2] - base[2]) as f32,
        ],
        point,
    })
}

/// Slab method: where the ray enters a box, and by which face.
fn enters(origin: [f64; 3], direction: [f64; 3], shape: &Aabb) -> Option<(f64, u8)> {
    let mut enter = 0.0f64;
    let mut exit = f64::INFINITY;
    let mut face = 0u8;

    for axis in 0..3 {
        if direction[axis].abs() < 1e-12 {
            if origin[axis] < shape.min[axis] || origin[axis] > shape.max[axis] {
                return None;
            }
            continue;
        }
        let inverse = 1.0 / direction[axis];
        let mut near = (shape.min[axis] - origin[axis]) * inverse;
        let mut far = (shape.max[axis] - origin[axis]) * inverse;
        // Entering by the far side when travelling backwards along the axis.
        let negative = near > far;
        if negative {
            std::mem::swap(&mut near, &mut far);
        }
        if near > enter {
            enter = near;
            // Travelling along an axis you enter by its low face; travelling
            // against it, by its high one. The game orders faces down, up,
            // north, south, west, east, so each axis is a pair.
            let pair = match axis {
                1 => 0, // down and up
                2 => 2, // north and south
                _ => 4, // west and east
            };
            face = pair + u8::from(negative);
        }
        exit = exit.min(far);
        if enter > exit {
            return None;
        }
    }
    (exit >= 0.0).then_some((enter, face))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Cubes;
    impl BlockShapes for Cubes {
        fn collision(&self, state: StateId) -> &[Aabb] {
            const FULL: [Aabb; 1] = [Aabb { min: [0.0; 3], max: [1.0; 3] }];
            if state.0 == 1 { &FULL } else { &[] }
        }
    }
    /// A single block at the origin.
    struct One;
    impl BlockView for One {
        fn state_at(&self, x: i32, y: i32, z: i32) -> StateId {
            if [x, y, z] == [0, 0, 0] { StateId(1) } else { StateId(0) }
        }
    }

    #[test]
    fn a_ray_down_the_x_axis_hits_the_west_face() {
        let hit = cast([-5.0, 0.5, 0.5], [1.0, 0.0, 0.0], 10.0, &One, &Cubes).expect("should hit");
        assert_eq!(hit.block, [0, 0, 0]);
        assert_eq!(hit.face, 4, "entering from the west");
        assert_eq!(hit.placement(), [-1, 0, 0], "a block placed there goes west of it");
    }

    #[test]
    fn a_ray_from_above_hits_the_top() {
        let hit = cast([0.5, 5.0, 0.5], [0.0, -1.0, 0.0], 10.0, &One, &Cubes).expect("should hit");
        assert_eq!(hit.face, 1);
        assert!((hit.point[1] - 1.0).abs() < 1e-9, "landed at {}", hit.point[1]);
        assert_eq!(hit.placement(), [0, 1, 0]);
    }

    #[test]
    fn reach_is_respected() {
        assert!(cast([-5.0, 0.5, 0.5], [1.0, 0.0, 0.0], 4.5, &One, &Cubes).is_none());
    }

    #[test]
    fn a_ray_that_misses_finds_nothing() {
        assert!(cast([-5.0, 9.5, 0.5], [1.0, 0.0, 0.0], 100.0, &One, &Cubes).is_none());
    }

    #[test]
    fn the_cursor_says_where_on_the_face() {
        let hit = cast([-5.0, 0.25, 0.75], [1.0, 0.0, 0.0], 10.0, &One, &Cubes).expect("hit");
        assert!((hit.cursor[1] - 0.25).abs() < 1e-6, "cursor {:?}", hit.cursor);
        assert!((hit.cursor[2] - 0.75).abs() < 1e-6, "cursor {:?}", hit.cursor);
    }

    #[test]
    fn a_diagonal_ray_does_not_slip_past_a_corner() {
        // The block is only clipped, which fixed-step sampling misses.
        let d = 1.0 / 2.0f64.sqrt();
        let hit = cast([-3.0, 0.5, -3.0 + 0.9], [d, 0.0, d], 10.0, &One, &Cubes);
        assert!(hit.is_some(), "the walk should visit every block the ray crosses");
    }
}
