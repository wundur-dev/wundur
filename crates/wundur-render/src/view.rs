//! What an interactive viewer needs: a camera to turn around a model, and the drawing behind
//! a trait, so the CPU renderer here can be swapped for another (a GPU one) without the
//! viewer changing.

use glam::Vec3;
use image::RgbaImage;

use crate::gltf::Model;
use crate::pose::Scene;
use crate::raster::{self, Camera, Look};

/// The size and look of a frame to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameSpec {
    pub width: u32,
    pub height: u32,
    pub look: Look,
    /// The frame stays up (nothing is moving), so it may take longer for a cleaner picture.
    pub best: bool,
}

/// Draws one frame of a posed model.
pub trait FrameRenderer: Send {
    /// What it is, for the viewer to show ("CPU").
    fn name(&self) -> &'static str;
    /// `scene` (of `model`) from `cam`, as `spec` says.
    fn render(&mut self, model: &Model, scene: &Scene, cam: &Camera, spec: &FrameSpec) -> RgbaImage;
}

/// Software rendering on every core: no GPU, the same pixels `look_at_model` shows the agent.
#[derive(Default)]
pub struct CpuRenderer;

impl FrameRenderer for CpuRenderer {
    fn name(&self) -> &'static str {
        "CPU"
    }

    fn render(&mut self, model: &Model, scene: &Scene, cam: &Camera, spec: &FrameSpec) -> RgbaImage {
        let ss = if spec.best { 2 } else { 1 };
        let (w, h) = (spec.width.max(1) as usize, spec.height.max(1) as usize);
        let px = raster::draw(model, scene, cam, w * ss, h * ss, &spec.look);
        let mut img = RgbaImage::new(w as u32, h as u32);
        let n = (ss * ss) as f32;
        for y in 0..h {
            for x in 0..w {
                let mut c = Vec3::ZERO;
                for sy in 0..ss {
                    for sx in 0..ss {
                        c += px[(y * ss + sy) * w * ss + x * ss + sx];
                    }
                }
                let c = c / n * 255.0;
                img.put_pixel(x as u32, y as u32, image::Rgba([c.x.round() as u8, c.y.round() as u8, c.z.round() as u8, 255]));
            }
        }
        img
    }
}

/// A camera turning around a point: yaw and pitch in degrees (yaw 0 looks at the model's
/// front, from +Z; 90 at its left side), and how far away.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Orbit {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub target: Vec3,
}

impl Orbit {
    /// Looking at all of `scene` from (`yaw`, `pitch`) in a view `aspect` times as wide as
    /// tall.
    pub fn fit(scene: &Scene, yaw: f32, pitch: f32, aspect: f32) -> Orbit {
        let o = Orbit { yaw, pitch, distance: 1.0, target: scene.center() };
        let cam = crate::fit(scene, o.direction(), Vec3::Y, 0.8, aspect);
        Orbit { distance: (cam.eye - cam.target).length(), target: cam.target, ..o }
    }

    /// From the target toward the camera.
    pub fn direction(&self) -> Vec3 {
        let (y, p) = (self.yaw.to_radians(), self.pitch.clamp(-89.0, 89.0).to_radians());
        Vec3::new(y.sin() * p.cos(), p.sin(), y.cos() * p.cos())
    }

    pub fn camera(&self) -> Camera {
        Camera { eye: self.target + self.direction() * self.distance, target: self.target, up: Vec3::Y, fov: crate::FOV }
    }

    /// Turn by a drag of (`dx`, `dy`) pixels.
    pub fn turn(&mut self, dx: f32, dy: f32) {
        self.yaw = (self.yaw - dx * 0.4).rem_euclid(360.0);
        self.pitch = (self.pitch + dy * 0.4).clamp(-89.0, 89.0);
    }

    /// Move the target across the view by a drag of (`dx`, `dy`) pixels in a view `height`
    /// pixels tall.
    pub fn pan(&mut self, dx: f32, dy: f32, height: f32) {
        let cam = self.camera();
        let fwd = (cam.target - cam.eye).normalize();
        let right = fwd.cross(Vec3::Y).normalize();
        let up = right.cross(fwd);
        let per_px = 2.0 * self.distance * (crate::FOV * 0.5).tan() / height.max(1.0);
        self.target += (-right * dx + up * dy) * per_px;
    }

    /// Come closer (`steps` > 0) or move away.
    pub fn zoom(&mut self, steps: f32) {
        self.distance = (self.distance * (-steps * 0.1).exp()).max(1e-4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_scene() -> (Model, Scene) {
        let dir = std::env::temp_dir().join(format!("wundur-view-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = dir.join(format!("cube-{}.glb", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::write(&path, crate::test_cube([0.8, 0.1, 0.1], None)).unwrap();
        let model = crate::gltf::load(&path).unwrap();
        let scene = crate::pose::pose(&model, None);
        (model, scene)
    }

    #[test]
    fn the_cpu_renderer_draws_a_frame_of_the_size_asked_with_the_model_in_the_middle() {
        let (model, scene) = cube_scene();
        let orbit = Orbit::fit(&scene, 35.0, 12.0, 1.5);
        let img = CpuRenderer.render(&model, &scene, &orbit.camera(), &FrameSpec { width: 150, height: 100, look: Look { dark: true, floor: true }, best: false });
        assert_eq!(img.dimensions(), (150, 100));
        let mid = img.get_pixel(75, 50).0;
        assert!(mid[0] > mid[1] + 60, "red cube in the middle: {mid:?}");
        let corner = img.get_pixel(1, 1).0;
        assert!(corner[0] < 80 && corner[2] < 80, "dark backdrop: {corner:?}");
        let best = CpuRenderer.render(&model, &scene, &orbit.camera(), &FrameSpec { width: 150, height: 100, look: Look::default(), best: true });
        assert_eq!(best.dimensions(), (150, 100));
    }

    #[test]
    fn a_wide_view_frames_closer_than_a_tall_one() {
        let (_, scene) = cube_scene();
        let wide = Orbit::fit(&scene, 0.0, 0.0, 2.0);
        let tall = Orbit::fit(&scene, 0.0, 0.0, 0.4);
        assert!(tall.distance > wide.distance * 1.5, "{} vs {}", tall.distance, wide.distance);
    }

    #[test]
    fn the_orbit_turns_pans_and_zooms() {
        let (_, scene) = cube_scene();
        let mut o = Orbit::fit(&scene, 0.0, 0.0, 1.0);
        let d = o.distance;
        o.zoom(5.0);
        assert!(o.distance < d);
        o.turn(-100.0, 1000.0);
        assert!((o.yaw - 40.0).abs() < 1e-3 && o.pitch <= 89.0, "{o:?}");
        let t = o.target;
        o.pan(50.0, 0.0, 500.0);
        assert!((o.target - t).length() > 0.0);
    }
}
