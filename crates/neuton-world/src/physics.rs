//! Player movement and collision.
//!
//! Deliberately close to the game's own approach: motion is applied one axis at
//! a time against the boxes it would enter, so a player sliding along a wall
//! keeps the component of their movement that is not blocked instead of
//! stopping dead.

use neuton_blocks::StateId;

/// An axis-aligned box in world space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Aabb {
    pub fn new(min: [f64; 3], max: [f64; 3]) -> Self {
        Self { min, max }
    }

    /// A box centred on `x`/`z` at `position`, standing on `y`.
    pub fn player(position: [f64; 3], width: f64, height: f64) -> Self {
        let half = width / 2.0;
        Self {
            min: [position[0] - half, position[1], position[2] - half],
            max: [position[0] + half, position[1] + height, position[2] + half],
        }
    }

    pub fn offset(&self, delta: [f64; 3]) -> Self {
        Self {
            min: [self.min[0] + delta[0], self.min[1] + delta[1], self.min[2] + delta[2]],
            max: [self.max[0] + delta[0], self.max[1] + delta[1], self.max[2] + delta[2]],
        }
    }

    /// Grown to cover everywhere the box would pass through moving by `delta`.
    pub fn swept(&self, delta: [f64; 3]) -> Self {
        let mut out = *self;
        for axis in 0..3 {
            if delta[axis] < 0.0 {
                out.min[axis] += delta[axis];
            } else {
                out.max[axis] += delta[axis];
            }
        }
        out
    }

    /// True if the two boxes overlap on all three axes.
    ///
    /// Touching is not overlapping: a player standing exactly on a floor must
    /// not count as inside it.
    pub fn intersects(&self, other: &Aabb) -> bool {
        (0..3).all(|a| self.min[a] < other.max[a] && self.max[a] > other.min[a])
    }
}

/// Where the blocks are.
pub trait BlockView {
    fn state_at(&self, x: i32, y: i32, z: i32) -> StateId;
}

/// What a block state is shaped like, for walking into.
pub trait BlockShapes {
    /// Boxes in 0..1 block space. Empty means you pass straight through.
    fn collision(&self, state: StateId) -> &[Aabb];
}

/// What the server says the player is allowed to do.
#[derive(Debug, Clone, Copy)]
pub struct Abilities {
    pub may_fly: bool,
    pub flying: bool,
    pub instant_build: bool,
    pub invulnerable: bool,
    pub fly_speed: f32,
    pub walk_speed: f32,
}

impl Default for Abilities {
    fn default() -> Self {
        // Survival, until the server says otherwise.
        Self {
            may_fly: false,
            flying: false,
            instant_build: false,
            invulnerable: false,
            fly_speed: 0.05,
            walk_speed: 0.1,
        }
    }
}

/// The player's physical state.
#[derive(Debug, Clone)]
pub struct Body {
    /// Feet position.
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub on_ground: bool,
    /// Set while flying; gravity and ground friction stop applying.
    pub flying: bool,
}

impl Default for Body {
    fn default() -> Self {
        Self {
            position: [0.0, 80.0, 0.0],
            velocity: [0.0; 3],
            on_ground: false,
            flying: true,
        }
    }
}

/// What the player is asking for this tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct Input {
    /// Forward is positive, back negative, in the range -1..1.
    pub forward: f32,
    /// Right is positive.
    pub strafe: f32,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
    /// Where the player is looking, in degrees.
    pub yaw: f32,
}

pub const PLAYER_WIDTH: f64 = 0.6;
pub const PLAYER_HEIGHT: f64 = 1.8;
pub const EYE_HEIGHT: f64 = 1.62;

/// Blocks per second.
const WALK_SPEED: f64 = 4.317;
const SPRINT_SPEED: f64 = 5.612;
const SNEAK_SPEED: f64 = 1.3;
const FLY_SPEED: f64 = 10.9;
/// Blocks per second squared, matching the game's 32 blocks per second per
/// second closely enough that jumps feel right.
const GRAVITY: f64 = 32.0;
const TERMINAL_VELOCITY: f64 = 78.4;
/// Chosen so a jump clears one block with margin.
///
/// Higher than the closed-form figure would suggest, because stepping the
/// integration in fixed slices loses a little height on the way up.
const JUMP_SPEED: f64 = 9.2;
/// How high a step the player walks up without jumping.
const STEP_HEIGHT: f64 = 0.6;
/// How quickly the player reaches the speed they are asking for, per second.
///
/// On the ground you take hold quickly but not instantly; in the air you have
/// almost no say, which is what stops a jump from being a steering opportunity.
const GROUND_ACCELERATION: f64 = 18.0;
const AIR_ACCELERATION: f64 = 2.5;
const FLY_ACCELERATION: f64 = 9.0;

/// Advances the body by `dt` seconds.
pub fn step(
    body: &mut Body,
    input: Input,
    world: &dyn BlockView,
    shapes: &dyn BlockShapes,
    dt: f64,
) {
    let dt = dt.clamp(0.0, 0.1);

    // Desired horizontal motion, in world axes.
    let yaw = (input.yaw as f64).to_radians();
    let (sin, cos) = yaw.sin_cos();
    let speed = if body.flying {
        FLY_SPEED
    } else if input.sneak {
        SNEAK_SPEED
    } else if input.sprint {
        SPRINT_SPEED
    } else {
        WALK_SPEED
    };

    // Forward is where the camera looks; right is ninety degrees clockwise
    // from it, which on these axes is the negation of the usual cross product.
    let forward = [-sin, cos];
    let right = [-cos, -sin];
    let mut wish = [
        forward[0] * input.forward as f64 + right[0] * input.strafe as f64,
        forward[1] * input.forward as f64 + right[1] * input.strafe as f64,
    ];
    // Normalised, or moving diagonally would be faster than moving straight.
    let length = (wish[0] * wish[0] + wish[1] * wish[1]).sqrt();
    if length > 1.0 {
        wish[0] /= length;
        wish[1] /= length;
    }

    // Approach the target speed rather than snapping to it. Setting velocity
    // directly makes movement feel like a camera on rails: instant to start,
    // instant to stop, and nothing carries. Acceleration and drag are what make
    // it feel like a player.
    let target = [wish[0] * speed, wish[1] * speed];
    let rate = if body.flying {
        FLY_ACCELERATION
    } else if body.on_ground {
        GROUND_ACCELERATION
    } else {
        // Barely any control in mid-air, which is what stops a jump being a
        // steering opportunity.
        AIR_ACCELERATION
    };
    // Exponential approach, so the result does not depend on the tick length.
    let blend = 1.0 - (-rate * dt).exp();
    body.velocity[0] += (target[0] - body.velocity[0]) * blend;
    body.velocity[2] += (target[1] - body.velocity[2]) * blend;

    if body.flying {
        // Vertical is driven directly rather than by gravity, and smoothed the
        // same way as the horizontal axes.
        let target = if input.jump {
            FLY_SPEED
        } else if input.sneak {
            -FLY_SPEED
        } else {
            0.0
        };
        body.velocity[1] += (target - body.velocity[1]) * blend;
    } else {
        if input.jump && body.on_ground {
            body.velocity[1] = JUMP_SPEED;
            body.on_ground = false;
        }
        body.velocity[1] -= GRAVITY * dt;
        body.velocity[1] = body.velocity[1].max(-TERMINAL_VELOCITY);
    }

    let motion = [
        body.velocity[0] * dt,
        body.velocity[1] * dt,
        body.velocity[2] * dt,
    ];
    let moved = move_with_collision(body, motion, world, shapes);

    // Running into something takes the speed out of that axis, or the player
    // accumulates velocity into a wall and shoots off when it ends.
    for axis in [0usize, 2] {
        if (moved[axis] - motion[axis]).abs() > 1e-9 {
            body.velocity[axis] = 0.0;
        }
    }
    if (moved[1] - motion[1]).abs() > 1e-9 {
        if motion[1] < 0.0 {
            body.on_ground = true;
        }
        body.velocity[1] = 0.0;
    } else if motion[1] < 0.0 {
        body.on_ground = false;
    }
}

/// Applies motion one axis at a time, clipping against the world.
///
/// Returns what actually happened. Y first, so landing on a floor is settled
/// before deciding whether a horizontal step is possible.
fn move_with_collision(
    body: &mut Body,
    motion: [f64; 3],
    world: &dyn BlockView,
    shapes: &dyn BlockShapes,
) -> [f64; 3] {
    let box_at = |position: [f64; 3]| Aabb::player(position, PLAYER_WIDTH, PLAYER_HEIGHT);
    let start = box_at(body.position);
    let boxes = nearby(&start.swept(motion), world, shapes);

    let mut moved = [0.0f64; 3];
    let mut position = body.position;

    // Y, then the horizontal axes.
    for axis in [1usize, 0, 2] {
        if motion[axis] == 0.0 {
            continue;
        }
        let current = box_at(position);
        let allowed = clip(&current, motion[axis], axis, &boxes);
        position[axis] += allowed;
        moved[axis] = allowed;
    }

    // Walking into a low step: try again from one step up, and keep it only if
    // the player ends up standing on something.
    let blocked = (moved[0] - motion[0]).abs() > 1e-9 || (moved[2] - motion[2]).abs() > 1e-9;
    if blocked && body.on_ground && !body.flying {
        let mut stepped = body.position;
        let lift = clip(&box_at(stepped), STEP_HEIGHT, 1, &boxes);
        if lift > 1e-6 {
            stepped[1] += lift;
            for axis in [0usize, 2] {
                if motion[axis] == 0.0 {
                    continue;
                }
                let allowed = clip(&box_at(stepped), motion[axis], axis, &boxes);
                stepped[axis] += allowed;
            }
            // Settle back down onto whatever is under the new position.
            let drop = clip(&box_at(stepped), -lift, 1, &boxes);
            stepped[1] += drop;

            let gained = (stepped[0] - body.position[0]).abs() + (stepped[2] - body.position[2]).abs();
            let before = moved[0].abs() + moved[2].abs();
            if gained > before + 1e-9 {
                moved = [
                    stepped[0] - body.position[0],
                    stepped[1] - body.position[1],
                    stepped[2] - body.position[2],
                ];
                position = stepped;
            }
        }
    }

    body.position = position;
    moved
}

/// How far the box can move along one axis before it would enter something.
fn clip(from: &Aabb, delta: f64, axis: usize, boxes: &[Aabb]) -> f64 {
    let mut allowed = delta;
    for other in boxes {
        // Only boxes that overlap on the other two axes can block this one.
        let overlaps = (0..3)
            .filter(|a| *a != axis)
            .all(|a| from.min[a] < other.max[a] - 1e-9 && from.max[a] > other.min[a] + 1e-9);
        if !overlaps {
            continue;
        }
        if delta > 0.0 && from.max[axis] <= other.min[axis] + 1e-9 {
            allowed = allowed.min(other.min[axis] - from.max[axis]);
        } else if delta < 0.0 && from.min[axis] >= other.max[axis] - 1e-9 {
            allowed = allowed.max(other.max[axis] - from.min[axis]);
        }
    }
    // Never push the player backwards through something they were already
    // clear of.
    if delta > 0.0 { allowed.max(0.0) } else { allowed.min(0.0) }
}

/// Collision boxes overlapping a region, in world coordinates.
fn nearby(region: &Aabb, world: &dyn BlockView, shapes: &dyn BlockShapes) -> Vec<Aabb> {
    let mut out = Vec::new();
    let lo = [
        region.min[0].floor() as i32 - 1,
        region.min[1].floor() as i32 - 1,
        region.min[2].floor() as i32 - 1,
    ];
    let hi = [
        region.max[0].ceil() as i32 + 1,
        region.max[1].ceil() as i32 + 1,
        region.max[2].ceil() as i32 + 1,
    ];
    for y in lo[1]..=hi[1] {
        for z in lo[2]..=hi[2] {
            for x in lo[0]..=hi[0] {
                let state = world.state_at(x, y, z);
                if state.is_air() {
                    continue;
                }
                for shape in shapes.collision(state) {
                    let world_box = Aabb::new(
                        [
                            x as f64 + shape.min[0],
                            y as f64 + shape.min[1],
                            z as f64 + shape.min[2],
                        ],
                        [
                            x as f64 + shape.max[0],
                            y as f64 + shape.max[1],
                            z as f64 + shape.max[2],
                        ],
                    );
                    if world_box.intersects(region) {
                        out.push(world_box);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A floor at y=0 filling y in 0..1, and nothing else.
    struct Floor;
    impl BlockView for Floor {
        fn state_at(&self, _x: i32, y: i32, _z: i32) -> StateId {
            if y == 0 { StateId(1) } else { StateId(0) }
        }
    }
    /// A floor with a one-block wall along x=2.
    struct Wall;
    impl BlockView for Wall {
        fn state_at(&self, x: i32, y: i32, _z: i32) -> StateId {
            if y == 0 || (x == 2 && y == 1) { StateId(1) } else { StateId(0) }
        }
    }
    /// A floor, raised by half a block from x=2 onwards.
    struct Step;
    impl BlockView for Step {
        fn state_at(&self, x: i32, y: i32, _z: i32) -> StateId {
            if y == 0 {
                StateId(1)
            } else if x >= 2 && y == 1 {
                StateId(2)
            } else {
                StateId(0)
            }
        }
    }

    struct Cubes;
    impl BlockShapes for Cubes {
        fn collision(&self, state: StateId) -> &[Aabb] {
            const FULL: [Aabb; 1] =
                [Aabb { min: [0.0, 0.0, 0.0], max: [1.0, 1.0, 1.0] }];
            const HALF: [Aabb; 1] =
                [Aabb { min: [0.0, 0.0, 0.0], max: [1.0, 0.5, 1.0] }];
            match state.0 {
                1 => &FULL,
                2 => &HALF,
                _ => &[],
            }
        }
    }

    fn walker(x: f64, y: f64) -> Body {
        Body {
            position: [x, y, 0.5],
            velocity: [0.0; 3],
            on_ground: false,
            flying: false,
        }
    }

    /// Runs a number of 50 ms ticks.
    fn run(body: &mut Body, input: Input, world: &dyn BlockView, ticks: usize) {
        for _ in 0..ticks {
            step(body, input, world, &Cubes, 0.05);
        }
    }

    #[test]
    fn gravity_lands_the_player_on_the_floor() {
        let mut body = walker(0.5, 6.0);
        run(&mut body, Input::default(), &Floor, 60);
        assert!(body.on_ground, "should have landed");
        assert!(
            (body.position[1] - 1.0).abs() < 1e-6,
            "feet should rest on the block top: {}",
            body.position[1]
        );
        assert!(body.velocity[1].abs() < 1e-6, "vertical speed should be spent");
    }

    #[test]
    fn a_player_does_not_sink_through_the_world() {
        let mut body = walker(0.5, 1.0);
        body.on_ground = true;
        run(&mut body, Input::default(), &Floor, 200);
        assert!(body.position[1] >= 1.0 - 1e-6, "sank to {}", body.position[1]);
    }

    #[test]
    fn a_wall_stops_horizontal_movement() {
        let mut body = walker(0.5, 1.0);
        body.on_ground = true;
        // Facing east: yaw 270 looks along +X.
        let input = Input { forward: 1.0, yaw: 270.0, ..Default::default() };
        run(&mut body, input, &Wall, 40);

        // The wall occupies x in 2..3, and the player is 0.6 wide.
        assert!(
            body.position[0] <= 2.0 - PLAYER_WIDTH / 2.0 + 1e-6,
            "walked into the wall: {}",
            body.position[0]
        );
        assert!(body.position[0] > 0.5, "did not move at all");
    }

    #[test]
    fn a_low_step_is_walked_up_without_jumping() {
        let mut body = walker(0.5, 1.0);
        body.on_ground = true;
        let input = Input { forward: 1.0, yaw: 270.0, ..Default::default() };
        run(&mut body, input, &Step, 40);

        assert!(
            body.position[0] > 2.0,
            "should have stepped onto the block, stopped at {}",
            body.position[0]
        );
        assert!(
            (body.position[1] - 1.5).abs() < 1e-3,
            "should be standing on the step: {}",
            body.position[1]
        );
    }

    #[test]
    fn jumping_clears_a_full_block() {
        let mut body = walker(0.5, 1.0);
        body.on_ground = true;
        let input = Input { jump: true, yaw: 0.0, ..Default::default() };
        let mut highest = body.position[1];
        for _ in 0..30 {
            step(&mut body, input, &Floor, &Cubes, 0.05);
            highest = highest.max(body.position[1]);
        }
        assert!(highest >= 2.0, "jump only reached {highest}");
        assert!(highest < 2.6, "jump went far too high: {highest}");
    }

    #[test]
    fn flying_ignores_gravity() {
        let mut body = walker(0.5, 20.0);
        body.flying = true;
        run(&mut body, Input::default(), &Floor, 40);
        assert!((body.position[1] - 20.0).abs() < 1e-6, "drifted to {}", body.position[1]);
    }

    #[test]
    fn speed_builds_up_rather_than_appearing() {
        let mut body = walker(0.5, 1.0);
        body.on_ground = true;
        let input = Input { forward: 1.0, yaw: 270.0, ..Default::default() };

        // One tick in, the player is moving but nowhere near full speed.
        step(&mut body, input, &Floor, &Cubes, 1.0 / 60.0);
        let first = body.velocity[0].abs();
        assert!(first > 0.0, "should have started moving");
        assert!(first < WALK_SPEED * 0.5, "reached {first} in a single tick");

        // And after a moment it is there.
        run(&mut body, input, &Floor, 30);
        assert!(
            (body.velocity[0].abs() - WALK_SPEED).abs() < 0.1,
            "settled at {}",
            body.velocity[0].abs()
        );
    }

    #[test]
    fn letting_go_slides_to_a_stop() {
        let mut body = walker(0.5, 1.0);
        body.on_ground = true;
        run(&mut body, Input { forward: 1.0, yaw: 270.0, ..Default::default() }, &Floor, 30);
        let moving = body.velocity[0].abs();

        // One tick of no input does not stop a player dead.
        step(&mut body, Input { yaw: 270.0, ..Default::default() }, &Floor, &Cubes, 1.0 / 60.0);
        assert!(body.velocity[0].abs() > 0.0, "stopped instantly");
        assert!(body.velocity[0].abs() < moving, "did not slow down");

        // But they do come to rest.
        run(&mut body, Input { yaw: 270.0, ..Default::default() }, &Floor, 60);
        assert!(body.velocity[0].abs() < 0.05, "still drifting at {}", body.velocity[0]);
    }

    #[test]
    fn the_tick_length_does_not_change_the_result() {
        // Exponential approach rather than a fixed fraction per tick, so
        // halving the step does not halve the acceleration.
        let settle = |dt: f64, ticks: usize| {
            let mut b = walker(0.5, 1.0);
            b.on_ground = true;
            for _ in 0..ticks {
                step(&mut b, Input { forward: 1.0, yaw: 270.0, ..Default::default() }, &Floor, &Cubes, dt);
            }
            b.velocity[0].abs()
        };
        let coarse = settle(1.0 / 30.0, 6);
        let fine = settle(1.0 / 60.0, 12);
        assert!((coarse - fine).abs() < 0.05, "{coarse} against {fine}");
    }

    #[test]
    fn diagonal_movement_is_not_faster() {
        let straight = {
            let mut b = walker(0.5, 1.0);
            b.on_ground = true;
            run(&mut b, Input { forward: 1.0, yaw: 270.0, ..Default::default() }, &Floor, 20);
            b.position
        };
        let diagonal = {
            let mut b = walker(0.5, 1.0);
            b.on_ground = true;
            run(
                &mut b,
                Input { forward: 1.0, strafe: 1.0, yaw: 270.0, ..Default::default() },
                &Floor,
                20,
            );
            b.position
        };
        let distance = |p: [f64; 3], q: [f64; 3]| {
            ((p[0] - q[0]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
        };
        let start = [0.5, 1.0, 0.5];
        assert!(
            (distance(straight, start) - distance(diagonal, start)).abs() < 1e-6,
            "diagonal covered {} against {}",
            distance(diagonal, start),
            distance(straight, start)
        );
    }

    #[test]
    fn touching_is_not_overlapping() {
        // A player resting exactly on a floor must not count as inside it, or
        // they are pushed out every tick.
        let feet = Aabb::player([0.5, 1.0, 0.5], PLAYER_WIDTH, PLAYER_HEIGHT);
        let block = Aabb::new([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        assert!(!feet.intersects(&block));
    }
}
