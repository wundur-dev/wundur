//! `cargo run -p wundur-render --release --example export_obj -- model.glb out.zip`: the ZIP the
//! viewer's Mixamo button makes (OBJ, MTL and PNG textures).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (model, out) = (std::path::Path::new(&args[0]), std::path::Path::new(&args[1]));
    let m = wundur_render::gltf::load(model).expect("a model");
    let name = model.file_stem().unwrap().to_string_lossy().to_string();
    println!("{:?}", wundur_render::export::obj_zip(&m, &name, out).expect("exported"));
}
