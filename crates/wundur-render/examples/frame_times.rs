//! `cargo run -p wundur-render --release --example frame_times -- a.glb ...`: how long the
//! viewer's frames take (posing, a moving frame, a still one) at panel sizes.
use std::time::Instant;
use wundur_render::view::{CpuRenderer, FrameRenderer, FrameSpec, Orbit};

fn main() {
    for path in std::env::args().skip(1) {
        let t = Instant::now();
        let model = match wundur_render::gltf::load(path.as_ref()) {
            Ok(m) => m,
            Err(e) => {
                println!("{path}: {e}");
                continue;
            }
        };
        let load = t.elapsed();
        let anim = (!model.animations.is_empty()).then(|| (0, model.animations[0].duration * 0.5));
        let t = Instant::now();
        let scene = wundur_render::pose::pose(&model, anim);
        let pose = t.elapsed();
        let cam = Orbit::fit(&scene, 40.0, 15.0, 1.6).camera();
        let mut r = CpuRenderer;
        let look = wundur_render::Look { dark: false, floor: true };
        let mut line = format!("{} ({} tris): load {:?}, pose {:?}", path.rsplit('/').next().unwrap(), scene.tris.len(), load, pose);
        for (w, h, best) in [(800, 500, false), (1600, 1000, false), (1600, 1000, true)] {
            let _ = r.render(&model, &scene, &cam, &FrameSpec { width: w, height: h, look, best });
            let t = Instant::now();
            let n = 5;
            for _ in 0..n {
                let _ = r.render(&model, &scene, &cam, &FrameSpec { width: w, height: h, look, best });
            }
            line.push_str(&format!(", {w}x{h}{} {:.1}ms", if best { " best" } else { "" }, t.elapsed().as_secs_f64() * 1000.0 / n as f64));
        }
        println!("{line}");
    }
}
