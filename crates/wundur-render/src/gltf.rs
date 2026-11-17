//! Reading a glTF model (a .glb, or a .gltf with its files): its nodes, meshes, skins,
//! animations, materials and textures, and what it says about itself. Written for this
//! renderer, so it reads what generators and optimisers actually write (quantized and
//! meshopt-compressed geometry, WebP textures) and keeps going past what it can't read,
//! noting it.

use std::path::Path;

use base64::Engine as _;
use glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use serde_json::Value;

pub struct Model {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
    pub meshes: Vec<Mesh>,
    pub skins: Vec<Skin>,
    pub materials: Vec<Material>,
    /// By glTF image index; `None` when it couldn't be read.
    pub images: Vec<Option<Texture>>,
    pub animations: Vec<Animation>,
    pub facts: Facts,
}

pub struct Node {
    pub name: String,
    pub children: Vec<usize>,
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
    pub matrix: Option<Mat4>,
    pub mesh: Option<usize>,
    pub skin: Option<usize>,
}

impl Node {
    pub fn local(&self) -> Mat4 {
        self.matrix.unwrap_or_else(|| Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation))
    }
}

pub struct Mesh {
    pub prims: Vec<Prim>,
}

pub struct Prim {
    pub positions: Vec<Vec3>,
    pub normals: Option<Vec<Vec3>>,
    pub uv0: Option<Vec<Vec2>>,
    pub uv1: Option<Vec<Vec2>>,
    pub colors: Option<Vec<Vec4>>,
    pub joints: Option<Vec<[u16; 4]>>,
    pub weights: Option<Vec<Vec4>>,
    /// Triangles, three indices each (strips and fans unrolled).
    pub indices: Vec<u32>,
    pub material: Option<usize>,
}

pub struct Skin {
    pub joints: Vec<usize>,
    pub inverse_bind: Vec<Mat4>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Alpha {
    Opaque,
    Mask(f32),
    Blend,
}

#[derive(Clone)]
pub struct TexRef {
    /// The glTF image it shows (`None`: none that could be read).
    pub image: Option<usize>,
    pub tex_coord: usize,
    pub wrap: [u32; 2],
    /// KHR_texture_transform: offset, rotation, scale.
    pub transform: Option<(Vec2, f32, Vec2)>,
    /// Normal map scale or occlusion strength.
    pub amount: f32,
}

#[derive(Clone)]
pub struct Material {
    pub name: String,
    pub base: Vec4,
    pub base_tex: Option<TexRef>,
    pub metallic: f32,
    pub roughness: f32,
    pub mr_tex: Option<TexRef>,
    pub normal_tex: Option<TexRef>,
    pub occlusion_tex: Option<TexRef>,
    pub emissive: Vec3,
    pub emissive_tex: Option<TexRef>,
    pub alpha: Alpha,
    pub double_sided: bool,
    pub unlit: bool,
}

impl Default for Material {
    fn default() -> Self {
        Material {
            name: String::new(),
            base: Vec4::ONE,
            base_tex: None,
            metallic: 0.0,
            roughness: 0.6,
            mr_tex: None,
            normal_tex: None,
            occlusion_tex: None,
            emissive: Vec3::ZERO,
            emissive_tex: None,
            alpha: Alpha::Opaque,
            double_sided: false,
            unlit: false,
        }
    }
}

/// An image as RGBA8, with its mip levels (level 0 first).
pub struct Texture {
    pub levels: Vec<(u32, u32, Vec<u8>)>,
}

impl Texture {
    fn new(img: image::RgbaImage) -> Texture {
        let (mut w, mut h) = img.dimensions();
        let mut levels = vec![(w, h, img.into_raw())];
        while w > 1 || h > 1 {
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            let src = &levels.last().unwrap().2;
            let mut out = vec![0u8; (nw * nh * 4) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    for c in 0..4 {
                        let mut sum = 0u32;
                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            let sx = (x * 2 + dx).min(w - 1);
                            let sy = (y * 2 + dy).min(h - 1);
                            sum += src[((sy * w + sx) * 4 + c) as usize] as u32;
                        }
                        out[((y * nw + x) * 4 + c) as usize] = (sum / 4) as u8;
                    }
                }
            }
            levels.push((nw, nh, out));
            (w, h) = (nw, nh);
        }
        Texture { levels }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Path3 {
    Translation,
    Rotation,
    Scale,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Interp {
    Linear,
    Step,
    Cubic,
}

pub struct Channel {
    pub node: usize,
    pub path: Path3,
    pub interp: Interp,
    pub times: Vec<f32>,
    /// Flat: 3 or 4 per key (three times that for cubic splines: in-tangent, value, out-tangent).
    pub values: Vec<f32>,
}

pub struct Animation {
    pub name: String,
    pub duration: f32,
    pub channels: Vec<Channel>,
}

/// A texture as the facts list it.
pub struct TextureFact {
    pub image: usize,
    pub name: String,
    pub format: String,
    pub size: Option<(u32, u32)>,
    pub bytes: usize,
    pub uses: Vec<&'static str>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Facts {
    pub file_bytes: u64,
    pub generator: String,
    pub meshes: usize,
    pub prims: usize,
    pub materials: usize,
    pub textures: Vec<TextureFact>,
    pub joints: usize,
    pub morph_targets: usize,
    pub extensions_used: Vec<String>,
    pub extensions_required: Vec<String>,
    pub warnings: Vec<String>,
}

impl Facts {
    fn warn(&mut self, w: impl Into<String>) {
        let w = w.into();
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }
}

const SUPPORTED: &[&str] = &[
    "EXT_meshopt_compression",
    "KHR_meshopt_compression",
    "KHR_mesh_quantization",
    "EXT_texture_webp",
    "KHR_texture_transform",
    "KHR_materials_unlit",
    "KHR_materials_pbrSpecularGlossiness",
    "KHR_materials_emissive_strength",
];

/// Read the model at `path` (a .glb or .gltf).
pub fn load(path: &Path) -> Result<Model, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("couldn't read it: {e}"))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let (json, bin) = if bytes.starts_with(b"glTF") { split_glb(&bytes)? } else { (bytes.clone(), None) };
    let doc: Value = serde_json::from_slice(&json).map_err(|e| format!("not a glTF model ({e})"))?;
    let mut facts = Facts { file_bytes: bytes.len() as u64, ..Default::default() };
    facts.generator = doc["asset"]["generator"].as_str().unwrap_or("").to_string();
    let strings = |v: &Value| -> Vec<String> { v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect() };
    facts.extensions_used = strings(&doc["extensionsUsed"]);
    facts.extensions_required = strings(&doc["extensionsRequired"]);
    for e in &facts.extensions_required.clone() {
        if !SUPPORTED.contains(&e.as_str()) {
            let what = match e.as_str() {
                "KHR_draco_mesh_compression" => "Draco-compressed geometry, which this renderer can't read: those parts are missing from the picture",
                "KHR_texture_basisu" => "KTX2/Basis textures, which this renderer can't read: those textures show as magenta",
                _ => "an extension this renderer doesn't know: the picture may be wrong",
            };
            facts.warn(format!("needs {e}: {what}"));
        }
    }

    // Buffers, then buffer views (meshopt-compressed ones decoded here).
    let buffers: Vec<Vec<u8>> = arr(&doc["buffers"])
        .iter()
        .enumerate()
        .map(|(i, b)| match b["uri"].as_str() {
            None if i == 0 => bin.clone().unwrap_or_default(),
            None => Vec::new(),
            Some(uri) => match read_uri(uri, dir) {
                Ok(v) => v,
                Err(e) => {
                    let fallback = b["extensions"]["EXT_meshopt_compression"]["fallback"].as_bool().unwrap_or(false);
                    if !fallback {
                        facts.warn(format!("buffer {i} ({uri}) couldn't be read: {e}"));
                    }
                    Vec::new()
                }
            },
        })
        .collect();
    let views: Vec<Vec<u8>> = arr(&doc["bufferViews"])
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let ext = &v["extensions"];
            let mo = if ext["EXT_meshopt_compression"].is_object() { &ext["EXT_meshopt_compression"] } else { &ext["KHR_meshopt_compression"] };
            if mo.is_object() {
                match meshopt_view(mo, &buffers) {
                    Ok(d) => d,
                    Err(e) => {
                        facts.warn(format!("compressed buffer view {i} couldn't be decoded: {e}"));
                        Vec::new()
                    }
                }
            } else {
                let buf = &buffers[(us(&v["buffer"])).min(buffers.len().saturating_sub(1))];
                let off = us(&v["byteOffset"]);
                let len = us(&v["byteLength"]);
                buf.get(off..off + len).map(<[u8]>::to_vec).unwrap_or_default()
            }
        })
        .collect();
    let acc = Accessors { doc: &doc, views: &views };

    // Images: decoded lazily per use, but all of them listed.
    let images_doc = arr(&doc["images"]);
    let mut images: Vec<Option<Texture>> = Vec::new();
    for (i, im) in images_doc.iter().enumerate() {
        let declared = im["mimeType"].as_str().unwrap_or("").to_string();
        let (data, name) = if let Some(bv) = im["bufferView"].as_u64() {
            (Ok(views.get(bv as usize).cloned().unwrap_or_default()), im["name"].as_str().unwrap_or("").to_string())
        } else if let Some(uri) = im["uri"].as_str() {
            let name = if uri.starts_with("data:") { String::new() } else { uri.to_string() };
            (read_uri(uri, dir), name)
        } else {
            (Err("no data".to_string()), String::new())
        };
        let mut fact = TextureFact { image: i, name, format: declared.trim_start_matches("image/").to_string(), size: None, bytes: 0, uses: Vec::new(), error: None };
        let decoded = data.and_then(|d| {
            fact.bytes = d.len();
            let actual = image::guess_format(&d).ok();
            if let Some(f) = actual {
                let actual_name = f.extensions_str().first().copied().unwrap_or("?").replace("jpg", "jpeg");
                if fact.format.is_empty() {
                    fact.format = actual_name;
                } else if !declared.is_empty() && !declared.ends_with(&actual_name) {
                    facts.warn(format!("image {i} says it is {declared} but holds {actual_name}: some loaders will refuse it"));
                    fact.format = format!("{actual_name} (labelled {declared})");
                }
            }
            image::load_from_memory(&d).map_err(|e| e.to_string())
        });
        match decoded {
            Ok(img) => {
                let rgba = img.to_rgba8();
                fact.size = Some(rgba.dimensions());
                images.push(Some(Texture::new(rgba)));
            }
            Err(e) => {
                fact.error = Some(e);
                images.push(None);
            }
        }
        facts.textures.push(fact);
    }

    // Textures point at images (WebP through its extension).
    let textures_doc = arr(&doc["textures"]);
    let samplers = arr(&doc["samplers"]);
    let tex_ref = |info: &Value, use_: &'static str, facts: &mut Facts| -> Option<TexRef> {
        let index = info["index"].as_u64()? as usize;
        let t = textures_doc.get(index)?;
        let webp = t["extensions"]["EXT_texture_webp"]["source"].as_u64();
        let source = webp.or(t["source"].as_u64()).map(|s| s as usize);
        if webp.is_none()
            && let Some(s) = source
            && facts.textures.get(s).is_some_and(|f| f.format.starts_with("webp"))
        {
            facts.warn(format!("texture {index} ({use_}) is WebP but isn't marked with EXT_texture_webp: most loaders and engines won't show it"));
        }
        if t["extensions"]["KHR_texture_basisu"].is_object() && webp.is_none() && t["source"].is_null() {
            facts.warn(format!("texture {index} ({use_}) is KTX2/Basis only, which this renderer can't read"));
        }
        if let Some(s) = source
            && let Some(f) = facts.textures.get_mut(s)
            && !f.uses.contains(&use_)
        {
            f.uses.push(use_);
        }
        let sampler = t["sampler"].as_u64().and_then(|s| samplers.get(s as usize));
        let wrap = |k: &str| sampler.and_then(|s| s[k].as_u64()).unwrap_or(10497) as u32;
        let tr = &info["extensions"]["KHR_texture_transform"];
        let transform = tr.is_object().then(|| (vec2(&tr["offset"], Vec2::ZERO), tr["rotation"].as_f64().unwrap_or(0.0) as f32, vec2(&tr["scale"], Vec2::ONE)));
        let tex_coord = tr["texCoord"].as_u64().or(info["texCoord"].as_u64()).unwrap_or(0) as usize;
        let amount = info["scale"].as_f64().or(info["strength"].as_f64()).unwrap_or(1.0) as f32;
        Some(TexRef { image: source.filter(|s| images.get(*s).is_some_and(Option::is_some)), tex_coord, wrap: [wrap("wrapS"), wrap("wrapT")], transform, amount })
    };

    let mut materials = Vec::new();
    for (i, m) in arr(&doc["materials"]).iter().enumerate() {
        let pbr = &m["pbrMetallicRoughness"];
        let sg = &m["extensions"]["KHR_materials_pbrSpecularGlossiness"];
        let mut mat = Material { name: m["name"].as_str().unwrap_or("").to_string(), ..Default::default() };
        if sg.is_object() {
            mat.base = vec4(&sg["diffuseFactor"], Vec4::ONE);
            mat.base_tex = tex_ref(&sg["diffuseTexture"], "base colour", &mut facts);
            mat.metallic = 0.0;
            mat.roughness = 1.0 - sg["glossinessFactor"].as_f64().unwrap_or(1.0) as f32;
        } else {
            mat.base = vec4(&pbr["baseColorFactor"], Vec4::ONE);
            mat.base_tex = tex_ref(&pbr["baseColorTexture"], "base colour", &mut facts);
            mat.metallic = pbr["metallicFactor"].as_f64().unwrap_or(1.0) as f32;
            mat.roughness = pbr["roughnessFactor"].as_f64().unwrap_or(1.0) as f32;
            mat.mr_tex = tex_ref(&pbr["metallicRoughnessTexture"], "metallic-roughness", &mut facts);
        }
        mat.normal_tex = tex_ref(&m["normalTexture"], "normal", &mut facts);
        mat.occlusion_tex = tex_ref(&m["occlusionTexture"], "occlusion", &mut facts);
        mat.emissive_tex = tex_ref(&m["emissiveTexture"], "emissive", &mut facts);
        mat.emissive = vec3(&m["emissiveFactor"], Vec3::ZERO) * m["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"].as_f64().unwrap_or(1.0) as f32;
        mat.alpha = match m["alphaMode"].as_str() {
            Some("MASK") => Alpha::Mask(m["alphaCutoff"].as_f64().unwrap_or(0.5) as f32),
            Some("BLEND") => Alpha::Blend,
            _ => Alpha::Opaque,
        };
        mat.double_sided = m["doubleSided"].as_bool().unwrap_or(false);
        mat.unlit = m["extensions"]["KHR_materials_unlit"].is_object();
        let label = if mat.name.is_empty() { format!("material {i}") } else { format!("material {i} ({})", mat.name) };
        if !sg.is_object() && !mat.unlit && mat.metallic > 0.9 && mat.mr_tex.is_none() {
            facts.warn(format!(
                "{label} is fully metallic{} with no metallic-roughness texture: it looks dark or chrome in engines with no environment light",
                if pbr["metallicFactor"].is_null() { " (metallicFactor left out, which means 1)" } else { "" }
            ));
        }
        if mat.alpha == Alpha::Blend {
            facts.warn(format!("{label} is see-through (BLEND); drawn here as cut out at 50%"));
        }
        materials.push(mat);
    }
    facts.materials = materials.len();

    let mut meshes = Vec::new();
    for (mi, m) in arr(&doc["meshes"]).iter().enumerate() {
        let mut prims = Vec::new();
        for p in arr(&m["primitives"]) {
            facts.prims += 1;
            if p["extensions"]["KHR_draco_mesh_compression"].is_object() {
                facts.warn(format!("mesh {mi} is Draco-compressed, which this renderer can't read: it's missing from the picture"));
                continue;
            }
            let a = &p["attributes"];
            let Some(pos) = a["POSITION"].as_u64() else { continue };
            let positions: Vec<Vec3> = acc.read(pos as usize, &mut facts).chunks_exact(3).map(Vec3::from_slice).collect();
            if positions.is_empty() {
                continue;
            }
            let read_opt = |name: &str, facts: &mut Facts| a[name].as_u64().map(|i| acc.read(i as usize, facts));
            let normals = read_opt("NORMAL", &mut facts).map(|v| v.chunks_exact(3).map(Vec3::from_slice).collect::<Vec<_>>());
            let uv0 = read_opt("TEXCOORD_0", &mut facts).map(|v| v.chunks_exact(2).map(Vec2::from_slice).collect::<Vec<_>>());
            let uv1 = read_opt("TEXCOORD_1", &mut facts).map(|v| v.chunks_exact(2).map(Vec2::from_slice).collect::<Vec<_>>());
            let colors = a["COLOR_0"].as_u64().map(|i| {
                let n = acc.components(i as usize);
                acc.read(i as usize, &mut facts).chunks_exact(n.max(1)).map(|c| Vec4::new(c[0], c[1], c[2], if n == 4 { c[3] } else { 1.0 })).collect::<Vec<_>>()
            });
            let joints = read_opt("JOINTS_0", &mut facts).map(|v| v.chunks_exact(4).map(|c| [c[0] as u16, c[1] as u16, c[2] as u16, c[3] as u16]).collect::<Vec<_>>());
            let weights = read_opt("WEIGHTS_0", &mut facts).map(|v| v.chunks_exact(4).map(Vec4::from_slice).collect::<Vec<_>>());
            facts.morph_targets += arr(&p["targets"]).len();
            let n = positions.len() as u32;
            let raw: Vec<u32> = match p["indices"].as_u64() {
                Some(i) => acc.read(i as usize, &mut facts).iter().map(|&v| v as u32).collect(),
                None => (0..n).collect(),
            };
            let indices = match p["mode"].as_u64().unwrap_or(4) {
                4 => raw.chunks_exact(3).flatten().copied().collect(),
                5 => (2..raw.len()).flat_map(|i| if i % 2 == 0 { [raw[i - 2], raw[i - 1], raw[i]] } else { [raw[i - 1], raw[i - 2], raw[i]] }).collect(),
                6 => (2..raw.len()).flat_map(|i| [raw[0], raw[i - 1], raw[i]]).collect(),
                _ => Vec::new(),
            };
            let indices: Vec<u32> = indices.chunks_exact(3).filter(|t| t.iter().all(|&i| i < n)).flatten().copied().collect();
            let material = p["material"].as_u64().map(|m| m as usize).filter(|m| *m < materials.len());
            if uv0.is_none() && material.and_then(|m| materials[m].base_tex.as_ref()).is_some() {
                facts.warn(format!("mesh {mi} has a texture but no texture coordinates: it can't show"));
            }
            prims.push(Prim { positions, normals, uv0, uv1, colors, joints, weights, indices, material });
        }
        meshes.push(Mesh { prims });
    }
    facts.meshes = meshes.len();

    let nodes: Vec<Node> = arr(&doc["nodes"])
        .iter()
        .map(|n| Node {
            name: n["name"].as_str().unwrap_or("").to_string(),
            children: arr(&n["children"]).iter().filter_map(Value::as_u64).map(|c| c as usize).collect(),
            translation: vec3(&n["translation"], Vec3::ZERO),
            rotation: {
                let q = vec4(&n["rotation"], Vec4::new(0.0, 0.0, 0.0, 1.0));
                Quat::from_xyzw(q.x, q.y, q.z, q.w).normalize()
            },
            scale: vec3(&n["scale"], Vec3::ONE),
            matrix: n["matrix"].as_array().filter(|m| m.len() == 16).map(|m| {
                let f: Vec<f32> = m.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();
                Mat4::from_cols_slice(&f)
            }),
            mesh: n["mesh"].as_u64().map(|m| m as usize).filter(|m| *m < meshes.len()),
            skin: n["skin"].as_u64().map(|s| s as usize),
        })
        .collect();
    let scene = doc["scene"].as_u64().unwrap_or(0) as usize;
    let mut roots: Vec<usize> = arr(&doc["scenes"]).get(scene).map(|s| arr(&s["nodes"]).iter().filter_map(Value::as_u64).map(|n| n as usize).collect()).unwrap_or_default();
    if roots.is_empty() {
        let mut child = vec![false; nodes.len()];
        for n in &nodes {
            for &c in &n.children {
                if c < child.len() {
                    child[c] = true;
                }
            }
        }
        roots = (0..nodes.len()).filter(|&i| !child[i]).collect();
    }
    roots.retain(|&r| r < nodes.len());

    let skins: Vec<Skin> = arr(&doc["skins"])
        .iter()
        .map(|s| {
            let joints: Vec<usize> = arr(&s["joints"]).iter().filter_map(Value::as_u64).map(|j| j as usize).filter(|j| *j < nodes.len()).collect();
            let inverse_bind = match s["inverseBindMatrices"].as_u64() {
                Some(i) => acc.read(i as usize, &mut facts).chunks_exact(16).map(Mat4::from_cols_slice).collect(),
                None => vec![Mat4::IDENTITY; joints.len()],
            };
            Skin { joints, inverse_bind }
        })
        .collect();
    facts.joints = skins.iter().map(|s| s.joints.len()).max().unwrap_or(0);

    let mut animations = Vec::new();
    for (ai, a) in arr(&doc["animations"]).iter().enumerate() {
        let samplers = arr(&a["samplers"]);
        let mut channels = Vec::new();
        let mut duration = 0f32;
        for c in arr(&a["channels"]) {
            let Some(node) = c["target"]["node"].as_u64().map(|n| n as usize).filter(|n| *n < nodes.len()) else { continue };
            let path = match c["target"]["path"].as_str() {
                Some("translation") => Path3::Translation,
                Some("rotation") => Path3::Rotation,
                Some("scale") => Path3::Scale,
                _ => continue,
            };
            let Some(s) = c["sampler"].as_u64().and_then(|s| samplers.get(s as usize)) else { continue };
            let interp = match s["interpolation"].as_str() {
                Some("STEP") => Interp::Step,
                Some("CUBICSPLINE") => Interp::Cubic,
                _ => Interp::Linear,
            };
            let times = acc.read(us(&s["input"]), &mut facts);
            let values = acc.read(us(&s["output"]), &mut facts);
            if let Some(&last) = times.last() {
                duration = duration.max(last);
            }
            channels.push(Channel { node, path, interp, times, values });
        }
        let name = a["name"].as_str().filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| format!("animation {ai}"));
        animations.push(Animation { name, duration, channels });
    }

    Ok(Model { nodes, roots, meshes, skins, materials, images, animations, facts })
}

fn split_glb(b: &[u8]) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize);
    let total = u32_at(8).ok_or("a broken GLB")?.min(b.len());
    let (mut json, mut bin, mut off) = (None, None, 12);
    while off + 8 <= total {
        let (len, kind) = (u32_at(off).unwrap_or(0), u32_at(off + 4).unwrap_or(0));
        let data = b.get(off + 8..(off + 8 + len).min(total)).ok_or("a broken GLB")?.to_vec();
        match kind {
            0x4E4F534A => json = Some(data),
            0x004E4942 => bin = Some(data),
            _ => {}
        }
        off += 8 + len.div_ceil(4) * 4;
    }
    Ok((json.ok_or("a GLB with no JSON")?, bin))
}

fn read_uri(uri: &str, dir: &Path) -> Result<Vec<u8>, String> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let (_, data) = rest.split_once(";base64,").ok_or("a data URI that isn't base64")?;
        return base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| e.to_string());
    }
    let name = percent_decode(uri);
    let p = dir.join(&name);
    // Only files beside the model (no climbing out of its folder).
    if name.split(['/', '\\']).any(|part| part == "..") || Path::new(&name).is_absolute() {
        return Err("outside the model's folder".into());
    }
    std::fs::read(&p).map_err(|e| e.to_string())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn meshopt_view(mo: &Value, buffers: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let buf = buffers.get(us(&mo["buffer"])).ok_or("no such buffer")?;
    let off = us(&mo["byteOffset"]);
    let src = buf.get(off..off + us(&mo["byteLength"])).ok_or("past the end of its buffer")?;
    let (count, stride) = (us(&mo["count"]), us(&mo["byteStride"]));
    if stride == 0 || count.saturating_mul(stride) > 1 << 31 {
        return Err("a bad size".into());
    }
    let mut out = vec![0u8; count * stride];
    use meshopt::ffi;
    let rc = unsafe {
        match mo["mode"].as_str() {
            Some("TRIANGLES") => ffi::meshopt_decodeIndexBuffer(out.as_mut_ptr().cast(), count, stride, src.as_ptr(), src.len()),
            Some("INDICES") => ffi::meshopt_decodeIndexSequence(out.as_mut_ptr().cast(), count, stride, src.as_ptr(), src.len()),
            _ => ffi::meshopt_decodeVertexBuffer(out.as_mut_ptr().cast(), count, stride, src.as_ptr(), src.len()),
        }
    };
    if rc != 0 {
        return Err(format!("meshoptimizer said {rc}"));
    }
    unsafe {
        match mo["filter"].as_str() {
            Some("OCTAHEDRAL") => ffi::meshopt_decodeFilterOct(out.as_mut_ptr().cast(), count, stride),
            Some("QUATERNION") => ffi::meshopt_decodeFilterQuat(out.as_mut_ptr().cast(), count, stride),
            Some("EXPONENTIAL") => ffi::meshopt_decodeFilterExp(out.as_mut_ptr().cast(), count, stride),
            Some("COLOR") => ffi::meshopt_decodeFilterColor(out.as_mut_ptr().cast(), count, stride),
            _ => {}
        }
    }
    Ok(out)
}

struct Accessors<'a> {
    doc: &'a Value,
    views: &'a [Vec<u8>],
}

impl Accessors<'_> {
    fn components(&self, i: usize) -> usize {
        type_components(self.doc["accessors"][i]["type"].as_str().unwrap_or(""))
    }

    /// An accessor's values as floats (normalized ones scaled to 0..1 or -1..1), sparse
    /// substitutions applied.
    fn read(&self, i: usize, facts: &mut Facts) -> Vec<f32> {
        let a = &self.doc["accessors"][i];
        if !a.is_object() {
            facts.warn(format!("accessor {i} is missing"));
            return Vec::new();
        }
        let n = self.components(i);
        let count = us(&a["count"]);
        let ct = a["componentType"].as_u64().unwrap_or(5126) as u32;
        let norm = a["normalized"].as_bool().unwrap_or(false);
        let mut out = vec![0f32; count * n];
        if let Some(v) = a["bufferView"].as_u64() {
            let view = self.views.get(v as usize).map(Vec::as_slice).unwrap_or(&[]);
            let csize = comp_size(ct);
            let stride = self.doc["bufferViews"][v as usize]["byteStride"].as_u64().map(|s| s as usize).filter(|s| *s > 0).unwrap_or(csize * n);
            let off = us(&a["byteOffset"]);
            for e in 0..count {
                for c in 0..n {
                    let at = off + e * stride + c * csize;
                    let Some(bytes) = view.get(at..at + csize) else {
                        facts.warn(format!("accessor {i} runs past its data"));
                        return out;
                    };
                    out[e * n + c] = comp(bytes, ct, norm);
                }
            }
        }
        let sp = &a["sparse"];
        if sp.is_object() {
            let sc = us(&sp["count"]);
            let (iv, vv) = (&sp["indices"], &sp["values"]);
            let iview = self.views.get(us(&iv["bufferView"])).map(Vec::as_slice).unwrap_or(&[]);
            let vview = self.views.get(us(&vv["bufferView"])).map(Vec::as_slice).unwrap_or(&[]);
            let ict = iv["componentType"].as_u64().unwrap_or(5125) as u32;
            let (isz, vsz) = (comp_size(ict), comp_size(ct));
            for k in 0..sc {
                let ia = us(&iv["byteOffset"]) + k * isz;
                let Some(ib) = iview.get(ia..ia + isz) else { break };
                let target = comp(ib, ict, false) as usize;
                for c in 0..n {
                    let va = us(&vv["byteOffset"]) + (k * n + c) * vsz;
                    if let (Some(vb), Some(slot)) = (vview.get(va..va + vsz), out.get_mut(target * n + c)) {
                        *slot = comp(vb, ct, norm);
                    }
                }
            }
        }
        out
    }
}

fn type_components(t: &str) -> usize {
    match t {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" | "MAT2" => 4,
        "MAT3" => 9,
        "MAT4" => 16,
        _ => 1,
    }
}

fn comp_size(ct: u32) -> usize {
    match ct {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        _ => 4,
    }
}

fn comp(b: &[u8], ct: u32, norm: bool) -> f32 {
    match ct {
        5120 => {
            let v = b[0] as i8 as f32;
            if norm { (v / 127.0).max(-1.0) } else { v }
        }
        5121 => {
            let v = b[0] as f32;
            if norm { v / 255.0 } else { v }
        }
        5122 => {
            let v = i16::from_le_bytes([b[0], b[1]]) as f32;
            if norm { (v / 32767.0).max(-1.0) } else { v }
        }
        5123 => {
            let v = u16::from_le_bytes([b[0], b[1]]) as f32;
            if norm { v / 65535.0 } else { v }
        }
        5125 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32,
        _ => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
    }
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn us(v: &Value) -> usize {
    v.as_u64().unwrap_or(0) as usize
}

fn floats(v: &Value) -> Vec<f32> {
    arr(v).iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect()
}

fn vec2(v: &Value, d: Vec2) -> Vec2 {
    let f = floats(v);
    if f.len() >= 2 { Vec2::new(f[0], f[1]) } else { d }
}

fn vec3(v: &Value, d: Vec3) -> Vec3 {
    let f = floats(v);
    if f.len() >= 3 { Vec3::new(f[0], f[1], f[2]) } else { d }
}

fn vec4(v: &Value, d: Vec4) -> Vec4 {
    let f = floats(v);
    if f.len() >= 4 { Vec4::new(f[0], f[1], f[2], f[3]) } else { d }
}
