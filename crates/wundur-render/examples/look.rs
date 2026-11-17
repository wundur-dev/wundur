//! `cargo run -p wundur-render --release --example look -- out.png a.glb [b.glb ...]`, with
//! `LOOK_VIEWS`, `LOOK_ANIM`, `LOOK_TIME`, `LOOK_FOCUS`, `LOOK_SAME` to try options.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (out, models) = args.split_first().expect("out.png model.glb...");
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let req = wundur_render::Request {
        models: models
            .iter()
            .map(|m| wundur_render::Item {
                path: m.into(),
                label: std::path::Path::new(m).file_stem().unwrap().to_string_lossy().into(),
                shown_path: m.clone(),
                facing: env("LOOK_FACING"),
            })
            .collect(),
        views: env("LOOK_VIEWS").map(|v| v.split(',').filter_map(wundur_render::View::parse).collect()).unwrap_or_default(),
        animation: env("LOOK_ANIM"),
        time: env("LOOK_TIME"),
        focus: env("LOOK_FOCUS"),
        zoom: 3.0,
        same_scale: env("LOOK_SAME").is_some(),
        cell: None,
    };
    let t = std::time::Instant::now();
    let sheet = wundur_render::render(&req);
    eprintln!("rendered in {:?}", t.elapsed());
    sheet.image.save(out).unwrap();
    println!("{}", sheet.report);
}
