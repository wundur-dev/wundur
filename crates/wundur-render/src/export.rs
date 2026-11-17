//! Exports for other tools. [`obj_zip`]: a model as a ZIP of an OBJ, its MTL and its base
//! colour textures as PNG, the form Mixamo (and most tools) take: the rest pose, no skeleton.

use std::io::Write as _;
use std::path::Path;

use crate::gltf::Model;
use crate::pose;

/// What an export wrote.
#[derive(Debug)]
pub struct Exported {
    pub triangles: usize,
    pub textures: usize,
    /// What didn't come along (a texture that couldn't be read, ...).
    pub notes: Vec<String>,
}

/// Write `model` (in its rest pose) to `out` as a ZIP holding `<name>.obj`, `<name>.mtl` and
/// a PNG per base colour texture.
pub fn obj_zip(model: &Model, name: &str, out: &Path) -> Result<Exported, String> {
    let scene = pose::pose(model, None);
    if scene.tris.is_empty() {
        return Err("it has no triangles to export".into());
    }
    let mut notes = Vec::new();
    // Materials as the triangles use them (None: glTF's default).
    let mut used: Vec<Option<u32>> = scene.tri_mat.clone();
    used.sort();
    used.dedup();
    let mat_name = |m: Option<u32>| match m {
        Some(i) => format!("mat{i}"),
        None => "default".to_string(),
    };

    // MTL, and the textures it names.
    let mut mtl = String::from("# Exported by Wundur\n");
    let mut pngs: Vec<(String, Vec<u8>)> = Vec::new();
    for &m in &used {
        mtl.push_str(&format!("\nnewmtl {}\n", mat_name(m)));
        let material = m.and_then(|i| model.materials.get(i as usize));
        let base = material.map(|mat| mat.base).unwrap_or(glam::Vec4::ONE);
        mtl.push_str(&format!("Kd {:.4} {:.4} {:.4}\nKa 0 0 0\nKs 0 0 0\nd {:.4}\nillum 1\n", base.x, base.y, base.z, base.w));
        let Some(tex) = material.and_then(|mat| mat.base_tex.as_ref()) else { continue };
        let Some(image) = tex.image else {
            notes.push(format!("{}'s base colour texture couldn't be read, so it isn't in the export", mat_name(m)));
            continue;
        };
        let file = format!("{name}_tex{image}.png");
        if !pngs.iter().any(|(f, _)| f == &file) {
            let Some(Some(t)) = model.images.get(image) else { continue };
            let (w, h, px) = &t.levels[0];
            let img = image::RgbaImage::from_raw(*w, *h, px.clone()).ok_or("a texture of the wrong size")?;
            let mut bytes = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).map_err(|e| e.to_string())?;
            pngs.push((file.clone(), bytes));
        }
        mtl.push_str(&format!("map_Kd {file}\n"));
        if tex.transform.is_some() {
            notes.push(format!("{}'s texture transform isn't carried over", mat_name(m)));
        }
    }

    // OBJ: every vertex once, faces grouped by material. OBJ's texture v runs up the image,
    // glTF's down it.
    let mut obj = format!("# Exported by Wundur\nmtllib {name}.mtl\no {name}\n");
    for p in &scene.pos {
        obj.push_str(&format!("v {:.6} {:.6} {:.6}\n", p.x, p.y, p.z));
    }
    for t in &scene.uv0 {
        obj.push_str(&format!("vt {:.6} {:.6}\n", t.x, 1.0 - t.y));
    }
    for n in &scene.nrm {
        obj.push_str(&format!("vn {:.5} {:.5} {:.5}\n", n.x, n.y, n.z));
    }
    for &m in &used {
        obj.push_str(&format!("usemtl {}\n", mat_name(m)));
        for (t, tri) in scene.tris.iter().enumerate() {
            if scene.tri_mat[t] != m {
                continue;
            }
            let [a, b, c] = tri.map(|i| i + 1);
            obj.push_str(&format!("f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}\n"));
        }
    }

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::create(out).map_err(|e| format!("couldn't write {}: {e}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut put = |file: &str, bytes: &[u8]| -> Result<(), String> {
        zip.start_file(file, opts).map_err(|e| e.to_string())?;
        zip.write_all(bytes).map_err(|e| e.to_string())
    };
    put(&format!("{name}.obj"), obj.as_bytes())?;
    put(&format!("{name}.mtl"), mtl.as_bytes())?;
    for (f, bytes) in &pngs {
        put(f, bytes)?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(Exported { triangles: scene.tris.len(), textures: pngs.len(), notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_becomes_an_obj_with_its_material_and_texture_in_a_zip() {
        let dir = std::env::temp_dir().join(format!("wundur-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A 2×2 PNG as the cube's texture.
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 200, 30, 255])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        std::fs::write(dir.join("cube.glb"), crate::test_cube([1.0, 0.5, 0.25], Some(&png))).unwrap();
        let model = crate::gltf::load(&dir.join("cube.glb")).unwrap();
        let out = dir.join("out/cube-mixamo.zip");
        let done = obj_zip(&model, "cube", &out).unwrap();
        assert_eq!((done.triangles, done.textures), (12, 1));
        assert!(done.notes.is_empty(), "{:?}", done.notes);
        let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let names: Vec<String> = z.file_names().filter_map(Result::ok).map(|n| n.to_string()).collect();
        assert!(names.contains(&"cube.obj".to_string()) && names.contains(&"cube.mtl".to_string()) && names.contains(&"cube_tex0.png".to_string()), "{names:?}");
        let mut obj = String::new();
        std::io::Read::read_to_string(&mut z.by_name("cube.obj").unwrap(), &mut obj).unwrap();
        assert_eq!(obj.lines().filter(|l| l.starts_with("v ")).count(), 24);
        assert_eq!(obj.lines().filter(|l| l.starts_with("f ")).count(), 12);
        assert!(obj.contains("mtllib cube.mtl") && obj.contains("usemtl mat0"));
        let mut mtl = String::new();
        std::io::Read::read_to_string(&mut z.by_name("cube.mtl").unwrap(), &mut mtl).unwrap();
        assert!(mtl.contains("map_Kd cube_tex0.png") && mtl.contains("Kd 1.0000 0.5000 0.2500"), "{mtl}");
        let tex = image::load_from_memory(&{
            let mut b = Vec::new();
            std::io::Read::read_to_end(&mut z.by_name("cube_tex0.png").unwrap(), &mut b).unwrap();
            b
        })
        .unwrap();
        assert_eq!(tex.to_rgba8().get_pixel(0, 0).0, [10, 200, 30, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
