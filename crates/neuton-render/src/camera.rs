//! Camera and the matrices it produces.
//!
//! The maths is written out rather than pulled in. It is one perspective
//! projection and one look-at, both of which are short, and a linear algebra
//! crate would be a dependency carried for about forty lines.

/// Column-major 4x4, the layout WGSL expects.
pub type Mat4 = [[f32; 4]; 4];

/// A free-flying camera.
#[derive(Debug, Clone)]
pub struct Camera {
    pub position: [f32; 3],
    /// Degrees, clockwise from south, matching Minecraft's convention.
    pub yaw: f32,
    /// Degrees, negative looking up.
    pub pitch: f32,
    pub fov_degrees: f32,
    pub near: f32,
    pub far: f32,
    pub aspect: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            position: [0.0, 80.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            fov_degrees: 70.0,
            near: 0.05,
            // Far enough for a large render distance; the depth buffer is
            // reversed so precision does not suffer for it.
            far: 2000.0,
            aspect: 16.0 / 9.0,
        }
    }
}

impl Camera {
    /// Unit vector the camera is looking along.
    ///
    /// Minecraft's yaw is zero facing +Z and increases clockwise, and pitch is
    /// negative looking up. Matching that means the numbers here read the same
    /// as the ones the server sends.
    pub fn forward(&self) -> [f32; 3] {
        let (yaw, pitch) = (self.yaw.to_radians(), self.pitch.to_radians());
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        [-sy * cp, -sp, cy * cp]
    }

    /// Unit vector to the camera's right, on the horizontal plane.
    ///
    /// This is `forward x up`, not `up x forward`. Facing south, right is west,
    /// and getting the sign backwards swaps A and D without failing any test
    /// that only checks the two are perpendicular.
    pub fn right(&self) -> [f32; 3] {
        let yaw = self.yaw.to_radians();
        let (sy, cy) = yaw.sin_cos();
        [-cy, 0.0, -sy]
    }

    pub fn view(&self) -> Mat4 {
        look_to(self.position, self.forward(), [0.0, 1.0, 0.0])
    }

    pub fn projection(&self) -> Mat4 {
        perspective(self.fov_degrees.to_radians(), self.aspect.max(0.001), self.near, self.far)
    }

    /// Projection times view, which is what the shader actually needs.
    pub fn view_projection(&self) -> Mat4 {
        mul(self.projection(), self.view())
    }

    /// Moves relative to where the camera is looking.
    pub fn fly(&mut self, forward: f32, right: f32, up: f32) {
        let f = self.forward();
        let r = self.right();
        for i in 0..3 {
            self.position[i] += f[i] * forward + r[i] * right;
        }
        self.position[1] += up;
    }

    /// Applies mouse movement, keeping pitch inside the range that does not
    /// flip the view over.
    pub fn turn(&mut self, dyaw: f32, dpitch: f32) {
        self.yaw = (self.yaw + dyaw).rem_euclid(360.0);
        self.pitch = (self.pitch + dpitch).clamp(-89.9, 89.9);
    }
}

/// The six planes bounding what the camera can see.
///
/// Chunks outside it are skipped entirely. At a normal field of view that is
/// most of the world: the camera sees perhaps a third of the chunks loaded
/// around it, and the rest cost nothing but a plane test each.
#[derive(Debug, Clone, Copy)]
pub struct Frustum {
    /// Each plane as `[a, b, c, d]`, with the normal pointing inwards.
    planes: [[f32; 4]; 6],
}

impl Frustum {
    /// Extracts the planes from a view-projection matrix.
    ///
    /// Adding or subtracting a row of the matrix from its w row gives a plane
    /// directly, which avoids inverting anything.
    pub fn from_matrix(m: Mat4) -> Self {
        let row = |i: usize| [m[0][i], m[1][i], m[2][i], m[3][i]];
        let (w, x, y, z) = (row(3), row(0), row(1), row(2));
        let combine = |a: [f32; 4], b: [f32; 4], add: bool| {
            let s = if add { 1.0 } else { -1.0 };
            let p = [a[0] + s * b[0], a[1] + s * b[1], a[2] + s * b[2], a[3] + s * b[3]];
            // Normalised so the distance test is a real distance, which lets a
            // caller give the sphere or box a margin in world units.
            let len = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            if len < 1e-9 { p } else { [p[0] / len, p[1] / len, p[2] / len, p[3] / len] }
        };
        Self {
            planes: [
                combine(w, x, true),  // left
                combine(w, x, false), // right
                combine(w, y, true),  // bottom
                combine(w, y, false), // top
                // Depth runs 0..1, so the near plane is the z row alone.
                combine(z, z, false).map(|_| 0.0),
                combine(w, z, false), // far
            ],
        }
        .with_near(z)
    }

    fn with_near(mut self, z_row: [f32; 4]) -> Self {
        let len = (z_row[0] * z_row[0] + z_row[1] * z_row[1] + z_row[2] * z_row[2]).sqrt();
        self.planes[4] = if len < 1e-9 {
            z_row
        } else {
            [z_row[0] / len, z_row[1] / len, z_row[2] / len, z_row[3] / len]
        };
        self
    }

    /// True if any part of the box is inside.
    ///
    /// Conservative: a box straddling two planes' outsides without being inside
    /// either can pass, which costs a draw call and never drops geometry.
    pub fn intersects(&self, min: [f32; 3], max: [f32; 3]) -> bool {
        for plane in &self.planes {
            // The corner furthest along the plane normal. If even that is
            // behind, every corner is.
            let far = [
                if plane[0] >= 0.0 { max[0] } else { min[0] },
                if plane[1] >= 0.0 { max[1] } else { min[1] },
                if plane[2] >= 0.0 { max[2] } else { min[2] },
            ];
            if plane[0] * far[0] + plane[1] * far[1] + plane[2] * far[2] + plane[3] < 0.0 {
                return false;
            }
        }
        true
    }
}

impl Camera {
    pub fn frustum(&self) -> Frustum {
        Frustum::from_matrix(self.view_projection())
    }
}

/// Right-handed perspective projection mapping depth to 0..1, which is what
/// wgpu expects.
pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fov_y / 2.0).tan();
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), -1.0],
        [0.0, 0.0, near * far / (near - far), 0.0],
    ]
}

/// View matrix from a position and a direction.
pub fn look_to(eye: [f32; 3], dir: [f32; 3], up: [f32; 3]) -> Mat4 {
    let f = normalize(dir);
    let s = normalize(cross(f, up));
    let u = cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [-dot(s, eye), -dot(u, eye), dot(f, eye), 1.0],
    ]
}

pub fn mul(a: Mat4, b: Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, cell) in col.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

/// Applies a matrix to a point, dividing through by w.
pub fn transform(m: Mat4, p: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 4];
    for (r, cell) in out.iter_mut().enumerate() {
        *cell = m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r];
    }
    let w = if out[3].abs() < 1e-9 { 1.0 } else { out[3] };
    [out[0] / w, out[1] / w, out[2] / w]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-9 { v } else { [v[0] / len, v[1] / len, v[2] / len] }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < tol)
    }

    #[test]
    fn yaw_follows_minecrafts_convention() {
        let mut c = Camera { yaw: 0.0, pitch: 0.0, ..Default::default() };
        // Yaw 0 looks along +Z, and increasing yaw turns clockwise seen from
        // above, which means towards -X.
        assert!(close(c.forward(), [0.0, 0.0, 1.0], 1e-5), "{:?}", c.forward());
        c.yaw = 90.0;
        assert!(close(c.forward(), [-1.0, 0.0, 0.0], 1e-5), "{:?}", c.forward());
        c.yaw = 180.0;
        assert!(close(c.forward(), [0.0, 0.0, -1.0], 1e-5), "{:?}", c.forward());
    }

    #[test]
    fn negative_pitch_looks_up() {
        let c = Camera { yaw: 0.0, pitch: -90.0, ..Default::default() };
        assert!(close(c.forward(), [0.0, 1.0, 0.0], 1e-5), "{:?}", c.forward());
    }

    #[test]
    fn pitch_cannot_flip_the_view_over() {
        let mut c = Camera::default();
        c.turn(0.0, -500.0);
        assert!(c.pitch > -90.0);
        c.turn(0.0, 1000.0);
        assert!(c.pitch < 90.0);
        // Yaw wraps rather than growing without bound.
        c.turn(720.0 + 45.0, 0.0);
        assert!((0.0..360.0).contains(&c.yaw), "{}", c.yaw);
    }

    #[test]
    fn a_point_ahead_projects_to_the_centre_of_the_screen() {
        let c = Camera { position: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, ..Default::default() };
        let p = transform(c.view_projection(), [0.0, 0.0, 10.0]);
        assert!(p[0].abs() < 1e-4 && p[1].abs() < 1e-4, "not centred: {p:?}");
        // In front of the camera, so inside the depth range.
        assert!((0.0..1.0).contains(&p[2]), "depth outside 0..1: {}", p[2]);
    }

    #[test]
    fn something_behind_the_camera_lands_outside_the_depth_range() {
        let c = Camera { position: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, ..Default::default() };
        let p = transform(c.view_projection(), [0.0, 0.0, -10.0]);
        assert!(!(0.0..=1.0).contains(&p[2]), "behind the camera must be clipped: {}", p[2]);
    }

    #[test]
    fn right_is_ninety_degrees_from_forward() {
        for yaw in [0.0, 37.0, 90.0, 200.0, 359.0] {
            let c = Camera { yaw, pitch: 0.0, ..Default::default() };
            assert!(dot(c.forward(), c.right()).abs() < 1e-5, "yaw {yaw}");
        }
    }

    #[test]
    fn right_points_right_and_not_left() {
        // Perpendicularity alone is true of both signs, which is how a swapped
        // pair of strafe keys passes a test suite.
        let c = Camera { yaw: 0.0, pitch: 0.0, ..Default::default() };
        // Facing south (+Z), your right hand points west (-X).
        assert!(close(c.right(), [-1.0, 0.0, 0.0], 1e-5), "{:?}", c.right());

        // And it agrees with the cross product it is shorthand for.
        for yaw in [0.0, 45.0, 137.0, 300.0] {
            let c = Camera { yaw, pitch: 0.0, ..Default::default() };
            let expected = cross(c.forward(), [0.0, 1.0, 0.0]);
            assert!(close(c.right(), expected, 1e-5), "yaw {yaw}: {:?}", c.right());
        }
    }

    #[test]
    fn strafing_right_moves_the_way_you_would_expect() {
        // Facing south, strafing right takes you west.
        let mut c = Camera { position: [0.0, 64.0, 0.0], yaw: 0.0, pitch: 0.0, ..Default::default() };
        c.fly(0.0, 3.0, 0.0);
        assert!(close(c.position, [-3.0, 64.0, 0.0], 1e-4), "{:?}", c.position);

        // Facing east, strafing right takes you south.
        let mut c = Camera { position: [0.0, 64.0, 0.0], yaw: 270.0, pitch: 0.0, ..Default::default() };
        c.fly(0.0, 3.0, 0.0);
        assert!(close(c.position, [0.0, 64.0, 3.0], 1e-3), "{:?}", c.position);
    }

    #[test]
    fn flying_forward_moves_along_the_view_direction() {
        let mut c = Camera { position: [0.0, 64.0, 0.0], yaw: 0.0, pitch: 0.0, ..Default::default() };
        c.fly(5.0, 0.0, 0.0);
        assert!(close(c.position, [0.0, 64.0, 5.0], 1e-4), "{:?}", c.position);
        c.fly(0.0, 0.0, 2.0);
        assert!((c.position[1] - 66.0).abs() < 1e-4);
    }

    #[test]
    fn the_frustum_keeps_what_is_in_front_and_drops_what_is_behind() {
        let c = Camera {
            position: [0.0, 64.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            aspect: 16.0 / 9.0,
            ..Default::default()
        };
        let f = c.frustum();

        // Straight ahead, well inside the far plane.
        assert!(f.intersects([-8.0, 56.0, 20.0], [8.0, 72.0, 36.0]), "ahead");
        // Directly behind.
        assert!(!f.intersects([-8.0, 56.0, -40.0], [8.0, 72.0, -24.0]), "behind");
        // Far off to the side.
        assert!(!f.intersects([400.0, 56.0, 20.0], [416.0, 72.0, 36.0]), "beside");
        // Beyond the far plane.
        assert!(!f.intersects([-8.0, 56.0, 5000.0], [8.0, 72.0, 5016.0]), "past far");
        // Enclosing the camera itself.
        assert!(f.intersects([-16.0, 48.0, -16.0], [16.0, 80.0, 16.0]), "around the camera");
    }

    #[test]
    fn turning_the_camera_changes_what_survives() {
        let mut c = Camera { position: [0.0, 64.0, 0.0], pitch: 0.0, ..Default::default() };
        let ahead = ([-8.0, 56.0, 20.0], [8.0, 72.0, 36.0]);

        c.yaw = 0.0;
        assert!(c.frustum().intersects(ahead.0, ahead.1));
        c.yaw = 180.0;
        assert!(!c.frustum().intersects(ahead.0, ahead.1), "should be behind now");
    }

    #[test]
    fn a_column_tall_enough_to_straddle_the_view_still_passes() {
        // A chunk is 384 blocks tall; the camera sees a slice of it, and
        // dropping the whole column because its centre is out of view would
        // punch holes in the world.
        let c = Camera { position: [0.0, 64.0, 0.0], yaw: 0.0, ..Default::default() };
        assert!(c.frustum().intersects([-8.0, -64.0, 20.0], [8.0, 320.0, 36.0]));
    }

    #[test]
    fn matrix_multiplication_matches_applying_them_in_turn() {
        let c = Camera { position: [3.0, 5.0, 7.0], yaw: 31.0, pitch: 12.0, ..Default::default() };
        let point = [10.0, 4.0, -2.0];
        let combined = transform(c.view_projection(), point);
        let stepwise = transform(c.projection(), transform(c.view(), point));
        // Not bit-identical, since the stepwise path divides by w twice.
        assert!(close(combined, stepwise, 1e-3), "{combined:?} vs {stepwise:?}");
    }
}
