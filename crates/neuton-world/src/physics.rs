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

    /// Boxes the crosshair picks against, and the selection box is drawn
    /// around. The game keeps this separate from collision and it is not a
    /// nicety: a fence is outlined one block tall and walked into one and a
    /// half, so picking against the collision box would put the selection
    /// half a block above the fence.
    fn outline(&self, state: StateId) -> &[Aabb] {
        self.collision(state)
    }

    /// How much grip a block gives underfoot. Ordinary blocks give 0.6.
    fn friction(&self, _state: StateId) -> f64 {
        DEFAULT_FRICTION
    }
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
    /// The movement speed attribute, as the server sets it. A tenth of a block
    /// per tick is the ordinary value.
    pub walk_speed: f64,
    /// The flying speed the server allows.
    pub fly_speed: f64,
    /// Ticks left before another jump is allowed.
    pub jump_delay: u8,
}

impl Default for Body {
    fn default() -> Self {
        Self {
            position: [0.0, 80.0, 0.0],
            velocity: [0.0; 3],
            on_ground: false,
            flying: true,
            walk_speed: 0.1,
            fly_speed: 0.05,
            jump_delay: 0,
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

/// One tick, in seconds. Everything below is expressed per tick, because the
/// game is: the server simulates twenty times a second and predicts the client
/// on the same schedule, so a client that integrates at any other rate walks a
/// curve the server did not expect.
pub const TICK: f64 = 0.05;

/// Downward acceleration, blocks per tick per tick.
const GRAVITY: f64 = 0.08;
/// Vertical drag, applied after gravity. Terminal velocity falls out of these
/// two at 3.92 blocks per tick.
const DRAG: f64 = 0.98;
/// Horizontal drag in the air, and the base the ground friction multiplies.
const AIR_DRAG: f64 = 0.91;
/// The friction of ordinary blocks. Ice is slipperier, slime less so.
pub const DEFAULT_FRICTION: f64 = 0.6;
/// Ties ground acceleration to friction so that walking on any surface reaches
/// the same top speed, just at a different rate.
const ACCELERATION_BASE: f64 = 0.216_000_02;
/// How much of a hold you have in mid-air. Almost none, which is what stops a
/// jump from being a steering opportunity.
const AIR_CONTROL: f64 = 0.02;
const AIR_CONTROL_SPRINTING: f64 = 0.026;
/// Upward velocity of a jump, blocks per tick.
const JUMP_POWER: f64 = 0.42;
/// A jump taken while sprinting is thrown forward as well as up.
const SPRINT_JUMP_BOOST: f64 = 0.2;
/// Sprinting raises the movement speed attribute by this much.
const SPRINT_MULTIPLIER: f64 = 1.3;
/// Sneaking scales the input rather than the speed.
const SNEAK_MULTIPLIER: f64 = 0.3;
/// How high a step the player walks up without jumping.
const STEP_HEIGHT: f64 = 0.6;
/// Ticks before another jump is allowed, as the game counts them.
const JUMP_DELAY: u8 = 10;

/// Advances the body by one tick.
///
/// The order here is the game's order, and it matters: motion is applied first,
/// then gravity and drag are folded in for the tick after. Applying them the
/// other way round produces a jump that is a few hundredths of a block short,
/// which is enough for a server to disagree with every step you take.
pub fn step(body: &mut Body, input: Input, world: &dyn BlockView, shapes: &dyn BlockShapes) {
    // Sneaking slows the input, not the speed, so a sneaking player still
    // accelerates at the walking rate towards a slower target.
    let scale = if input.sneak && !body.flying { SNEAK_MULTIPLIER } else { 1.0 };
    let mut wish = [input.strafe as f64 * scale, input.forward as f64 * scale];
    // Normalised only if it is over one, so a half-pressed stick stays half.
    let length_squared = wish[0] * wish[0] + wish[1] * wish[1];
    if length_squared > 1.0 {
        let length = length_squared.sqrt();
        wish[0] /= length;
        wish[1] /= length;
    }

    // Whether the player was standing on something when the tick began. The
    // game decides friction from this once, up front, and keeps using it even
    // if the tick ends in the air. That is why a jump still pays the ground's
    // friction: take it from the end of the tick instead and a sprint jump
    // keeps almost all of its speed, every hop compounds, and the player ends
    // up moving at nearly twice the pace the server expects.
    let grounded = body.on_ground;

    // The block underfoot decides how fast you take hold of the ground. It is
    // sampled a little below the feet, as the game does, so standing exactly on
    // a boundary picks the block you are standing on rather than the air.
    let friction = if grounded {
        let below = [
            body.position[0].floor() as i32,
            (body.position[1] - 0.500_000_1).floor() as i32,
            body.position[2].floor() as i32,
        ];
        shapes.friction(world.state_at(below[0], below[1], below[2]))
    } else {
        1.0
    };

    let acceleration = if body.flying {
        body.fly_speed * if input.sprint { 2.0 } else { 1.0 }
    } else if grounded {
        let speed = body.walk_speed * if input.sprint { SPRINT_MULTIPLIER } else { 1.0 };
        speed * (ACCELERATION_BASE / (friction * friction * friction))
    } else if input.sprint {
        AIR_CONTROL_SPRINTING
    } else {
        AIR_CONTROL
    };

    // A jump is taken before the tick moves, off the ground state the last tick
    // ended on. The delay after one is the game's, and stops a held jump key
    // from firing again the instant a landing is registered.
    body.jump_delay = body.jump_delay.saturating_sub(1);
    if input.jump && grounded && !body.flying && body.jump_delay == 0 {
        body.jump_delay = JUMP_DELAY;
        body.velocity[1] = JUMP_POWER;
        if input.sprint {
            let yaw = (input.yaw as f64).to_radians();
            body.velocity[0] -= yaw.sin() * SPRINT_JUMP_BOOST;
            body.velocity[2] += yaw.cos() * SPRINT_JUMP_BOOST;
        }
        body.on_ground = false;
    }
    if !input.jump {
        // Letting go clears the wait, so tapping is never slower than holding.
        body.jump_delay = 0;
    }
    if body.flying {
        let up = f64::from(input.jump) - f64::from(input.sneak);
        if up != 0.0 {
            body.velocity[1] += up * body.fly_speed * 3.0;
        }
    }

    // Forward is where the camera looks; right is a quarter turn clockwise from
    // it. Both axes are built from the same pair of terms, and getting one sign
    // wrong does not tilt the strafe slightly -- it reflects it, so at forty
    // five degrees "right" points backwards and A and D become another pair of
    // forward and back keys.
    let yaw = (input.yaw as f64).to_radians();
    let (sin, cos) = yaw.sin_cos();
    let forward = [-sin, cos];
    let right = [-cos, -sin];
    body.velocity[0] += (forward[0] * wish[1] + right[0] * wish[0]) * acceleration;
    body.velocity[2] += (forward[1] * wish[1] + right[1] * wish[0]) * acceleration;

    let motion = body.velocity;
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

    // Gravity and drag land at the end of the tick, for the next one to use.
    if body.flying {
        body.velocity[1] *= 0.6;
        body.velocity[0] *= AIR_DRAG;
        body.velocity[2] *= AIR_DRAG;
    } else {
        body.velocity[1] = (body.velocity[1] - GRAVITY) * DRAG;
        let horizontal = if grounded { friction * AIR_DRAG } else { AIR_DRAG };
        body.velocity[0] *= horizontal;
        body.velocity[2] *= horizontal;
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
            walk_speed: 0.1,
            fly_speed: 0.05,
            jump_delay: 0,
        }
    }

    /// What walking flat out comes to, in blocks per tick. The game's 4.317
    /// blocks a second.
    const WALK_SPEED: f64 = 0.2158;

    /// How far one more tick actually carries the player, which is what the
    /// server measures. The stored velocity has already had drag taken off it.
    fn step_distance(body: &mut Body, input: Input, world: &dyn BlockView) -> f64 {
        let before = body.position;
        step(body, input, world, &Cubes);
        let d = [body.position[0] - before[0], body.position[2] - before[2]];
        (d[0] * d[0] + d[1] * d[1]).sqrt()
    }

    /// Runs a number of 50 ms ticks.
    fn run(body: &mut Body, input: Input, world: &dyn BlockView, ticks: usize) {
        for _ in 0..ticks {
            step(body, input, world, &Cubes);
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
        // A player standing on the ground still carries one tick of gravity,
        // exactly as the game does: it is what keeps them pressed down.
        assert!(
            (body.velocity[1] + GRAVITY * DRAG).abs() < 1e-9,
            "resting speed was {}",
            body.velocity[1]
        );
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
            step(&mut body, input, &Floor, &Cubes);
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
        let before = body.position[0];
        step(&mut body, input, &Floor, &Cubes);
        let first = (body.position[0] - before).abs();
        assert!(first > 0.0, "should have started moving");
        assert!(first < WALK_SPEED * 0.6, "reached {first} in a single tick");

        // And after a moment it is there.
        run(&mut body, input, &Floor, 30);
        let settled = step_distance(&mut body, input, &Floor);
        assert!((settled - WALK_SPEED).abs() < 0.02, "settled at {settled}");
    }

    #[test]
    fn letting_go_slides_to_a_stop() {
        let mut body = walker(0.5, 1.0);
        body.on_ground = true;
        run(&mut body, Input { forward: 1.0, yaw: 270.0, ..Default::default() }, &Floor, 30);
        let moving = body.velocity[0].abs();

        // One tick of no input does not stop a player dead.
        step(&mut body, Input { yaw: 270.0, ..Default::default() }, &Floor, &Cubes);
        assert!(body.velocity[0].abs() > 0.0, "stopped instantly");
        assert!(body.velocity[0].abs() < moving, "did not slow down");

        // But they do come to rest.
        run(&mut body, Input { yaw: 270.0, ..Default::default() }, &Floor, 60);
        assert!(body.velocity[0].abs() < 0.05, "still drifting at {}", body.velocity[0]);
    }

    #[test]
    fn walking_settles_at_the_speed_the_game_walks_at() {
        // Not a round number anyone chose: it falls out of an acceleration of a
        // tenth of a block a tick against a drag of 0.6 * 0.91.
        let mut body = walker(0.5, 1.0);
        body.on_ground = true;
        let input = Input { forward: 1.0, yaw: 270.0, ..Default::default() };
        run(&mut body, input, &Floor, 60);
        let blocks_per_second = step_distance(&mut body, input, &Floor) / TICK;
        assert!(
            (blocks_per_second - 4.317).abs() < 0.15,
            "walked at {blocks_per_second} blocks a second"
        );
    }

    #[test]
    fn sprint_jumping_is_faster_than_sprinting_but_not_absurdly() {
        // The game's own numbers: sprinting is 5.612 blocks a second, and
        // bunny hopping carries you along at a bit under twice walking pace.
        // Anything near double sprinting means the jump boost is being applied
        // more than once a jump.
        let settle = |jump: bool| {
            let mut body = walker(0.5, 1.0);
            body.on_ground = true;
            let input =
                Input { forward: 1.0, yaw: 270.0, sprint: true, jump, ..Default::default() };
            run(&mut body, input, &Floor, 200);
            let before = body.position[0];
            run(&mut body, input, &Floor, 40);
            (body.position[0] - before).abs() / (40.0 * TICK)
        };
        let sprinting = settle(false);
        let hopping = settle(true);
        assert!(
            (sprinting - 5.612).abs() < 0.2,
            "sprinting settled at {sprinting} blocks a second"
        );
        assert!(
            hopping > sprinting,
            "sprint jumping ({hopping}) should beat sprinting ({sprinting})"
        );
        assert!(
            hopping < 9.0,
            "sprint jumping ran away at {hopping} blocks a second"
        );
    }

    #[test]
    fn sprinting_is_a_third_faster_than_walking() {
        let settle = |sprint: bool| {
            let mut body = walker(0.5, 1.0);
            body.on_ground = true;
            let input = Input { forward: 1.0, yaw: 270.0, sprint, ..Default::default() };
            run(&mut body, input, &Floor, 60);
            step_distance(&mut body, input, &Floor)
        };
        let ratio = settle(true) / settle(false);
        assert!((ratio - 1.3).abs() < 0.02, "sprint was {ratio} times walking");
    }

    #[test]
    fn falling_reaches_the_game_terminal_velocity() {
        // Gravity and drag together, not a clamp: 0.08 * 0.98 / 0.02.
        struct Void;
        impl BlockView for Void {
            fn state_at(&self, _x: i32, _y: i32, _z: i32) -> StateId {
                StateId(0)
            }
        }
        let mut body = walker(0.5, 500.0);
        run(&mut body, Input::default(), &Void, 400);
        let fell = body.position[1];
        step(&mut body, Input::default(), &Void, &Cubes);
        let per_tick = fell - body.position[1];
        assert!((per_tick - 3.92).abs() < 0.01, "fell at {per_tick} blocks a tick");
    }

    #[test]
    fn strafing_is_square_to_looking_at_every_angle() {
        // The old test only ever looked due west, where the reflected strafe
        // vector happens to coincide with the correct one. Every angle, then.
        for turn in 0..24 {
            let yaw = turn as f32 * 15.0;
            let direction = |forward: f32, strafe: f32| {
                let mut body = walker(0.5, 1.0);
                body.on_ground = true;
                let input = Input { forward, strafe, yaw, ..Default::default() };
                let before = body.position;
                step(&mut body, input, &Floor, &Cubes);
                [body.position[0] - before[0], body.position[2] - before[2]]
            };
            let ahead = direction(1.0, 0.0);
            let across = direction(0.0, 1.0);

            let dot = ahead[0] * across[0] + ahead[1] * across[1];
            assert!(dot.abs() < 1e-9, "at {yaw} degrees strafe is not square to forward");

            // Right, specifically, not left. Facing south, your right hand
            // points west, so forward crossed with right comes out positive on
            // these axes.
            let cross = ahead[0] * across[1] - ahead[1] * across[0];
            assert!(cross > 0.0, "at {yaw} degrees D walks left, not right");
        }
    }

    #[test]
    fn looking_south_walks_south() {
        // Yaw zero faces positive Z, and right of that is negative X.
        let mut body = walker(0.5, 1.0);
        body.on_ground = true;
        let before = body.position;
        step(&mut body, Input { forward: 1.0, yaw: 0.0, ..Default::default() }, &Floor, &Cubes);
        assert!(body.position[2] - before[2] > 0.0, "forward should go south");

        let mut body = walker(0.5, 1.0);
        body.on_ground = true;
        let before = body.position;
        step(&mut body, Input { strafe: 1.0, yaw: 0.0, ..Default::default() }, &Floor, &Cubes);
        assert!(body.position[0] - before[0] < 0.0, "right of south is west");
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


#[cfg(test)]
mod shape_tests {
    use crate::shapes;
    use neuton_blocks::StateId;

    /// The block that started this: an end rod's model narrows to two pixels
    /// above its base, but the box stays four pixels the whole way up. Reading
    /// collision off the model let a player walk a pixel closer on each side
    /// than the server allowed, and the server put them back.
    #[test]
    fn an_end_rod_is_four_pixels_all_the_way_up() {
        let rod = StateId(14640);
        let boxes = shapes::collision(rod);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].min, [0.375, 0.0, 0.375]);
        assert_eq!(boxes[0].max, [0.625, 1.0, 0.625]);
    }

    /// A fence is walked into half a block higher than it is drawn, which is
    /// what stops it being jumped, and is not something any model says.
    #[test]
    fn a_fence_is_taller_to_walk_into_than_to_look_at() {
        let fence = StateId(6996);
        assert_eq!(shapes::collision(fence)[0].max[1], 1.5);
        assert_eq!(shapes::outline(fence)[0].max[1], 1.0);
    }

    /// Walls too, and they are wider than a fence into the bargain.
    #[test]
    fn a_wall_is_taller_to_walk_into_than_to_look_at() {
        let wall = StateId(9984);
        assert_eq!(shapes::collision(wall)[0].max[1], 1.5);
        assert_eq!(shapes::outline(wall)[0].max[1], 1.0);
    }

    /// Air is walked through, and a state past the end of the table is too
    /// rather than panicking on a server that knows a block we do not.
    #[test]
    fn nothing_to_walk_into_where_there_is_nothing() {
        assert!(shapes::collision(StateId(0)).is_empty());
        assert!(shapes::collision(StateId(u32::MAX)).is_empty());
        assert!(shapes::outline(StateId(u32::MAX)).is_empty());
    }

    /// A plain block is still a plain block.
    #[test]
    fn stone_is_a_full_cube() {
        let stone = StateId(1);
        let boxes = shapes::collision(stone);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].min, [0.0, 0.0, 0.0]);
        assert_eq!(boxes[0].max, [1.0, 1.0, 1.0]);
    }
}
