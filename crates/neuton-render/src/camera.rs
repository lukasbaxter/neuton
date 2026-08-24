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
    pub fn right(&self) -> [f32; 3] {
        let yaw = self.yaw.to_radians();
        let (sy, cy) = yaw.sin_cos();
        [cy, 0.0, sy]
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
    fn flying_forward_moves_along_the_view_direction() {
        let mut c = Camera { position: [0.0, 64.0, 0.0], yaw: 0.0, pitch: 0.0, ..Default::default() };
        c.fly(5.0, 0.0, 0.0);
        assert!(close(c.position, [0.0, 64.0, 5.0], 1e-4), "{:?}", c.position);
        c.fly(0.0, 0.0, 2.0);
        assert!((c.position[1] - 66.0).abs() < 1e-4);
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
