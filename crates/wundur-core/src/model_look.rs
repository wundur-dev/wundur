//! Looking at 3D models: draw GLB/glTF models to one picture (a row per model, a column per
//! camera angle, the same camera and light for all) with their facts, through
//! `wundur-render`. Wundur's own `look_at_model` tool comes here, and packs reach the same
//! drawing through `kx.renderModel`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// What `look_at_model` answers: its text, and the picture when there is one.
#[derive(Debug, Default)]
pub struct Look {
    pub text: String,
    pub image: Option<PathBuf>,
    pub failed: bool,
}

/// Where pictures go in a project: Wundur's own folder, which keeps itself out of git.
pub const LOOKS: &str = ".wundur/looks";

/// The `look_at_model` tool: `args` as an agent gives them, for a session in `cwd`. Models
/// may be anywhere the agent can name (a path in the project, an absolute one, or `~/…`);
/// the picture goes in the project's `.wundur/looks/`.
pub fn look(cwd: &Path, args: &Value) -> Look {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    // Paths, one per line or comma-separated (or a list); "path @+x" says which way it faces.
    let entries: Vec<String> = match args.get("models").or(args.get("model")) {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect(),
        Some(Value::String(m)) => m.split([',', '\n']).map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect(),
        _ => Vec::new(),
    };
    if entries.is_empty() {
        return Look { text: "look_at_model needs models: one GLB or glTF path, or several separated by commas.".into(), failed: true, ..Default::default() };
    }
    let labels: Vec<String> = s("labels").map(|l| l.split(',').map(|x| x.trim().to_string()).collect()).unwrap_or_default();
    let mut models = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let (path, facing) = split_facing(entry);
        let lower = path.to_lowercase();
        if !lower.ends_with(".glb") && !lower.ends_with(".gltf") {
            return Look { text: format!("{path} isn't a GLB or glTF model."), failed: true, ..Default::default() };
        }
        let mut m = json!({ "path": path });
        if let Some(f) = facing {
            m["facing"] = json!(f);
        }
        if let Some(l) = labels.get(i).filter(|l| !l.is_empty()) {
            m["label"] = json!(l);
        }
        models.push(m);
    }
    let views: Vec<String> = s("views").map(|v| v.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
    let stem = if entries.len() == 1 {
        Path::new(split_facing(&entries[0]).0).file_stem().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()
    } else {
        "compare".to_string()
    };
    let name = slug(&s("name").unwrap_or_else(|| match s("focus") {
        Some(f) => format!("{stem}-{f}"),
        None => stem.clone(),
    }));
    let num = |k: &str| args.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())));
    let req = json!({
        "models": models,
        "views": views,
        "facing": s("facing"),
        "animation": s("animation"),
        "time": s("time"),
        "focus": s("focus"),
        "zoom": num("zoom"),
        "sameScale": args.get("same_scale").and_then(|v| v.as_bool().or_else(|| v.as_str().map(|t| t == "true"))).unwrap_or(false),
        "cell": num("size").map(|c| c as u64),
        "out": format!("{LOOKS}/{name}.png"),
    });
    match render(cwd, &req, &|p| anywhere(cwd, p)) {
        Ok((r, path)) => {
            let shown = if views.is_empty() { "front, left side, back, three-quarter".to_string() } else { views.join(", ") };
            let n = entries.len();
            let mut text = format!(
                "{} ({}×{}): {}; columns {}{}.",
                r["image"].as_str().unwrap_or_default(),
                r["width"],
                r["height"],
                if n == 1 { "one model".to_string() } else { format!("{n} models, a row each") },
                shown,
                s("focus").map(|f| format!(", close up on {f}")).unwrap_or_default()
            );
            let (failed, warnings) = (r["failed"].as_u64().unwrap_or(0), r["warnings"].as_u64().unwrap_or(0));
            if failed > 0 {
                text.push_str(&format!(" {failed} couldn't be loaded."));
            }
            if warnings > 0 {
                text.push_str(&format!(" {warnings} warning{}: see below.", if warnings == 1 { "" } else { "s" }));
            }
            text.push_str("\n\n");
            text.push_str(r["report"].as_str().unwrap_or_default());
            Look { text, image: Some(path), failed: false }
        }
        Err(e) => Look { text: format!("Couldn't draw it: {e}"), failed: true, ..Default::default() },
    }
}

/// "path @+x": the path, and the way it faces.
fn split_facing(entry: &str) -> (&str, Option<String>) {
    if let Some((p, f)) = entry.rsplit_once('@') {
        let f = f.trim().to_lowercase();
        if matches!(f.as_str(), "+x" | "-x" | "+z" | "-z" | "x" | "z") {
            return (p.trim(), Some(f));
        }
    }
    (entry.trim(), None)
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let s: String = out.trim_matches('-').chars().take(48).collect();
    if s.is_empty() { "model".into() } else { s }
}

/// A model path an agent names: in the project, absolute, or from the home folder.
fn anywhere(cwd: &Path, p: &str) -> Result<PathBuf, String> {
    let path = match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if Path::new(p).is_absolute() => PathBuf::from(p),
        _ => cwd.join(p),
    };
    Ok(path)
}

/// Draw the models `req` names (`models`: paths or {path, label, facing}; `views`,
/// `animation`, `time`, `focus`, `zoom`, `sameScale`, `cell`) as one sheet, and save it as a
/// PNG at `out`, a path in `cwd` (replacing what was there). `model_path` turns a model's
/// path into a file, or refuses it. The answer: the picture's facts as JSON, and where it is.
pub(crate) fn render(cwd: &Path, req: &Value, model_path: &dyn Fn(&str) -> Result<PathBuf, String>) -> Result<(Value, PathBuf), String> {
    let r = req;
    let mut models = Vec::new();
    for m in r["models"].as_array().into_iter().flatten() {
        let (path, label, facing) = match m {
            Value::String(p) => (p.as_str(), None, None),
            _ => (m["path"].as_str().unwrap_or(""), m["label"].as_str(), m["facing"].as_str()),
        };
        let full = model_path(path)?;
        if !full.is_file() {
            return Err(format!("{path}: no such file"));
        }
        let stem = full.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        models.push(wundur_render::Item {
            path: full,
            label: label.map(str::to_string).unwrap_or(stem),
            shown_path: path.to_string(),
            facing: facing.or(r["facing"].as_str()).map(str::to_string),
        });
    }
    if models.is_empty() {
        return Err("no models to draw".into());
    }
    let mut views = Vec::new();
    for v in r["views"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        views.push(wundur_render::View::parse(v).ok_or_else(|| format!("\"{v}\" isn't a view: front, back, left, right, three-quarter, back-three-quarter, top, bottom"))?);
    }
    let text = |k: &str| r[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let request = wundur_render::Request {
        models,
        views,
        animation: text("animation"),
        time: text("time"),
        focus: text("focus"),
        zoom: r["zoom"].as_f64().unwrap_or(3.0) as f32,
        same_scale: r["sameScale"].as_bool().unwrap_or(false),
        cell: r["cell"].as_u64().map(|c| c as u32),
    };
    let out_name = r["out"].as_str().ok_or("no out path")?;
    let out = crate::packs::inside(cwd, out_name)?;
    let sheet = wundur_render::render(&request);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Wundur's own folder in a project keeps itself out of git.
    let own = cwd.join(".wundur");
    if out.starts_with(&own) && !own.join(".gitignore").exists() {
        let _ = std::fs::write(own.join(".gitignore"), "*\n");
    }
    sheet.image.save_with_format(&out, image::ImageFormat::Png).map_err(|e| format!("couldn't save {out_name}: {e}"))?;
    let v = json!({
        "image": out_name,
        "width": sheet.image.width(),
        "height": sheet.image.height(),
        "report": sheet.report,
        "warnings": sheet.warnings,
        "failed": sheet.failed,
    });
    Ok((v, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wundur-look-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn look_at_model_draws_models_for_the_agent_and_keeps_the_picture_out_of_git() {
        let cwd = scratch("look");
        std::fs::create_dir_all(cwd.join("models")).unwrap();
        std::fs::write(cwd.join("models/red.glb"), wundur_render::test_cube([0.8, 0.1, 0.1], None)).unwrap();
        // A model outside the project, by its full path.
        let elsewhere = scratch("elsewhere").join("odd.glb");
        std::fs::write(&elsewhere, wundur_render::test_cube([1.0, 1.0, 1.0], Some(b"no image"))).unwrap();
        let models = format!("models/red.glb, {} @+x", elsewhere.display());
        let out = look(&cwd, &json!({ "models": models, "views": "front, top", "labels": "Red, Odd", "size": "128" }));
        assert!(!out.failed, "{}", out.text);
        let pic = cwd.join(".wundur/looks/compare.png");
        assert_eq!(out.image.as_deref(), Some(pic.as_path()));
        assert_eq!(image::image_dimensions(&pic).unwrap().0, 256, "two views of 128 across");
        assert!(out.text.starts_with(".wundur/looks/compare.png"), "{}", out.text);
        assert!(out.text.contains("2 models, a row each; columns front, top"), "{}", out.text);
        assert!(out.text.contains("Red (models/red.glb"), "{}", out.text);
        assert!(out.text.contains("taken as facing +x"), "{}", out.text);
        assert!(out.text.contains("WARNING: texture #0 (base colour) failed to load"), "{}", out.text);
        assert_eq!(std::fs::read_to_string(cwd.join(".wundur/.gitignore")).unwrap(), "*\n");
        // One model: named after it; a close-up adds the focus.
        let one = look(&cwd, &json!({ "models": ["models/red.glb"], "focus": "front top" }));
        assert_eq!(one.image, Some(cwd.join(".wundur/looks/red-front-top.png")), "{}", one.text);
        // Not a model, or not there.
        assert!(look(&cwd, &json!({ "models": "models/red.png" })).failed);
        assert!(look(&cwd, &json!({ "models": "models/gone.glb" })).failed);
        assert!(look(&cwd, &json!({})).failed);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_path_may_say_which_way_its_model_faces() {
        assert_eq!(split_facing("a/b.glb @+X"), ("a/b.glb", Some("+x".to_string())));
        assert_eq!(split_facing("a/b@2x.glb"), ("a/b@2x.glb", None));
        assert_eq!(slug("Raptor  v2 / front"), "raptor-v2-front");
    }
}
