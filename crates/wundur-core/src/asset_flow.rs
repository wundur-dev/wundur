//! The hosted asset flow (see `docs/proposals/hosted-asset-flow.md`): an asset made on the
//! person's Wundur account, step by step. Describe it (a kind from the catalog and its fields),
//! approve a set of views (the service decides how many), pick an engine and look at a preview,
//! reroll if needed, then Import, which is the only step that spends model credits.
//!
//! What's on offer comes from the service's catalog; how it's made is the service's business.
//! This module is the client: the types the API answers with (read leniently, so fields the
//! service adds later don't break an older app) and a function per call. All calls block: run
//! them off the UI thread.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::account::{self, Auth, Error};

/// A count as the service may send it: a whole number, a decimal (`30.0`) or a numeric string;
/// anything else reads as 0, so one odd field never fails the whole answer.
fn count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_f64().map_or(0, |f| f.max(0.0).round() as u32),
        Value::String(s) => s.trim().parse::<f64>().map_or(0, |f| f.max(0.0).round() as u32),
        _ => 0,
    })
}

/// A list as the service may send it: `null` reads as empty (an older record without the
/// field), so one such field never fails the whole answer.
fn list<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

/// [`count`], for a field that may be missing or null.
fn maybe_count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Null => None,
        Value::Number(n) => n.as_f64().map(|f| f.max(0.0).round() as u32),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f.max(0.0).round() as u32),
        _ => None,
    })
}

// ----- the catalog -----

/// What the service offers, from `GET /v1/catalog`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    pub version: String,
    #[serde(deserialize_with = "list")]
    pub kinds: Vec<Kind>,
    #[serde(deserialize_with = "list")]
    pub engines: Vec<Engine>,
    #[serde(deserialize_with = "list")]
    pub riggers: Vec<Rigger>,
    #[serde(deserialize_with = "list")]
    pub sounds: Vec<SoundEngine>,
    /// How detailed a model is made (triangles), for the Model step.
    #[serde(deserialize_with = "list")]
    pub details: Vec<Detail>,
    /// The detail picked to start with, and the one for a faceted (low poly) model.
    pub default_detail: String,
    pub faceted_detail: String,
}

/// A detail level for a model: how many triangles it is made with.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detail {
    pub id: String,
    pub label: String,
    #[serde(deserialize_with = "count")]
    pub triangles: u32,
    pub help: String,
}

/// A kind of thing to make (a humanoid, a creature, a prop, a place).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Kind {
    pub id: String,
    pub label: String,
    pub description: String,
    /// The form the person fills in, in order.
    #[serde(deserialize_with = "list")]
    pub fields: Vec<Field>,
    /// How many views it usually gets, for placeholders while they're made (the real number
    /// is whatever the set holds).
    #[serde(deserialize_with = "count")]
    pub views: u32,
    pub supports_3d: bool,
    /// The engines that can make it, by id.
    #[serde(deserialize_with = "list")]
    pub engines: Vec<String>,
    pub default_engine: String,
    /// What's offered instead of a model when it has none (`concept_art`).
    #[serde(deserialize_with = "list")]
    pub offers: Vec<String>,
}

/// One field of a kind's form.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Field {
    pub id: String,
    pub label: String,
    /// `text`, `enum`, `number` or `bool`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(deserialize_with = "list")]
    pub options: Vec<String>,
    pub default: Option<Value>,
    pub required: bool,
    pub help: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// A picture for each option (option → path under the API's base URL; public), for a
    /// choice shown as picture tiles (the art style). An option without one is a text tile.
    pub option_images: BTreeMap<String, String>,
    /// A line on what each option does (option → "Adult proportions."), shown under the
    /// choices for the one picked; without one the field's help is shown there.
    pub option_help: BTreeMap<String, String>,
}

impl Field {
    /// The line under the field's choices: what the picked option does, else the field's help.
    pub fn help_for(&self, picked: &str) -> &str {
        self.option_help.get(picked).map(String::as_str).filter(|h| !h.is_empty()).unwrap_or(&self.help)
    }
}

/// Who may use something, and how its output may be used.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct License {
    pub commercial: bool,
    /// Shown on the choice when it isn't cleared for everything ("Personal / non-commercial use
    /// only"); empty otherwise.
    pub label: String,
}

/// A 3D model engine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Engine {
    pub id: String,
    pub label: String,
    /// Model credits Import spends.
    #[serde(deserialize_with = "count")]
    pub credits: u32,
    /// The plans that may use it.
    #[serde(deserialize_with = "list")]
    pub plans: Vec<String>,
    /// About how long a model takes.
    #[serde(deserialize_with = "count")]
    pub seconds: u32,
    #[serde(deserialize_with = "list")]
    pub best_for: Vec<String>,
    pub help: String,
    pub license: License,
    /// Whether it's given every view of the approved set (else only the front).
    pub multiview: bool,
    /// The finishes it offers (`smooth`, `faceted`); most engines have none.
    #[serde(deserialize_with = "list")]
    pub finishes: Vec<String>,
    /// Whether this account may use it (the catalog is per caller); `None` from an API before
    /// the beta, which listed `plans` instead.
    pub available: Option<bool>,
}

/// A rigger, for an imported model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rigger {
    pub id: String,
    pub label: String,
    #[serde(deserialize_with = "count")]
    pub credits: u32,
    /// The kinds it rigs.
    #[serde(deserialize_with = "list")]
    pub kinds: Vec<String>,
    #[serde(deserialize_with = "count")]
    pub seconds: u32,
    pub help: String,
    pub license: License,
}

/// A sound effect maker.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundEngine {
    pub id: String,
    pub label: String,
    #[serde(deserialize_with = "count")]
    pub max_seconds: u32,
    pub license: License,
}

impl Catalog {
    pub fn kind(&self, id: &str) -> Option<&Kind> {
        self.kinds.iter().find(|k| k.id == id)
    }

    pub fn engine(&self, id: &str) -> Option<&Engine> {
        self.engines.iter().find(|e| e.id == id)
    }

    /// The engines that can make `kind`, in the catalog's order.
    pub fn engines_for(&self, kind: &Kind) -> Vec<&Engine> {
        kind.engines.iter().filter_map(|id| self.engine(id)).collect()
    }

    /// The riggers for `kind`.
    pub fn riggers_for(&self, kind: &str) -> Vec<&Rigger> {
        self.riggers.iter().filter(|r| r.kinds.is_empty() || r.kinds.iter().any(|k| k == kind)).collect()
    }

    pub fn detail(&self, id: &str) -> Option<&Detail> {
        self.details.iter().find(|d| d.id == id)
    }

    /// The detail to start with: the faceted one when the model will be faceted, else the
    /// default (else the first offered).
    pub fn detail_for(&self, faceted: bool) -> Option<&Detail> {
        let wanted = if faceted { &self.faceted_detail } else { &self.default_detail };
        self.detail(wanted).or_else(|| self.detail(&self.default_detail)).or_else(|| self.details.first())
    }
}

/// The field drawn as picture tiles at the top of the form (the art style), not among the
/// rest of a kind's settings.
pub const STYLE_FIELD: &str = "style";

/// One line saying what a kind's settings are set to, for the folded settings: each choice
/// with its field ("Adult height", "Closed mouth"; a choice of several words after its field:
/// "Stands on two legs"), fields with the same one-word choice together ("Auto hands & feet"),
/// switches that are on by name. The style and free text are left out.
pub fn settings_summary(fields: &[Field], choices: &BTreeMap<String, String>, flags: &BTreeMap<String, bool>) -> String {
    let cap = |t: &str| {
        let mut c = t.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
    };
    // (the one-word choice, the fields with it) or a phrase on its own.
    let mut parts: Vec<(Option<String>, Vec<String>)> = Vec::new();
    for f in fields.iter().filter(|f| f.id != STYLE_FIELD) {
        match f.kind.as_str() {
            "enum" => {
                let Some(value) = choices.get(&f.id).map(|v| v.trim()).filter(|v| !v.is_empty()) else { continue };
                if value.contains(' ') {
                    parts.push((None, vec![format!("{} {}", cap(&f.label), value.to_lowercase())]));
                } else {
                    let label = f.label.to_lowercase();
                    match parts.last_mut() {
                        Some((Some(v), labels)) if v.eq_ignore_ascii_case(value) => labels.push(label),
                        _ => parts.push((Some(value.to_string()), vec![label])),
                    }
                }
            }
            "bool" if flags.get(&f.id).copied().unwrap_or(false) => parts.push((None, vec![cap(&f.label)])),
            _ => {}
        }
    }
    parts
        .into_iter()
        .map(|(value, labels)| match value {
            Some(v) => {
                let joined = match labels.split_last() {
                    Some((last, rest)) if !rest.is_empty() => format!("{} & {last}", rest.join(", ")),
                    _ => labels.join(""),
                };
                format!("{} {joined}", cap(&v))
            }
            None => labels.join(""),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Whether an asset's style makes its model faceted by default (low poly).
pub fn low_poly(style: Option<&str>) -> bool {
    style.is_some_and(|s| s.eq_ignore_ascii_case("low poly"))
}

/// The looks the form offers, of the catalog's styles: the image engines draw realistic and
/// stylized well; low poly and toon are better made from the model (the Faceted finish, a toon
/// shader in the engine). A catalog offering neither keeps all of its own.
pub const LOOKS: [&str; 2] = ["stylized", "realistic"];

/// The options of a style field the form shows (see [`LOOKS`]).
pub fn looks(field: &Field) -> Vec<String> {
    let shown: Vec<String> = field.options.iter().filter(|o| LOOKS.iter().any(|l| o.eq_ignore_ascii_case(l))).cloned().collect();
    if shown.is_empty() { field.options.clone() } else { shown }
}

/// The finish to start with on `engine`: faceted for a low-poly asset, smooth otherwise, when
/// the engine offers it (else its first); none on an engine without finishes.
pub fn default_finish(engine: &Engine, low_poly: bool) -> Option<String> {
    let wanted = if low_poly { "faceted" } else { "smooth" };
    engine.finishes.iter().find(|f| *f == wanted).or_else(|| engine.finishes.first()).cloned()
}

/// A name for an asset from its description, to show greyed in the name box (the service
/// names it the same way when none is given): its first words, before what it wears or holds,
/// without a leading article. "A tiger raptor scout wearing a vest" → "Tiger raptor scout".
pub fn suggested_name(description: &str) -> String {
    let first = description.trim().split(['.', ',', ';', ':', '\n', '(']).next().unwrap_or_default();
    let stops = ["wearing", "with", "in", "holding", "carrying", "who", "that", "on", "and", "from", "made"];
    let mut words: Vec<&str> = Vec::new();
    for (n, w) in first.split_whitespace().enumerate() {
        let lower = w.to_lowercase();
        if n == 0 && matches!(lower.as_str(), "a" | "an" | "the") {
            continue;
        }
        if !words.is_empty() && stops.contains(&lower.as_str()) {
            break;
        }
        words.push(w);
        if words.len() == 5 {
            break;
        }
    }
    let name = words.join(" ");
    let mut chars = name.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

impl Engine {
    /// Whether the person may use it: as the catalog says (`available`), else whether `plan`
    /// is among its plans (an engine listing none is open to all).
    pub fn on_plan(&self, plan: &str) -> bool {
        self.available.unwrap_or_else(|| self.plans.is_empty() || self.plans.iter().any(|p| p == plan))
    }
}

// ----- assets, sets, models -----

/// An asset being made: its description and its rounds of views and models.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub fields: BTreeMap<String, Value>,
    pub created_at: String,
    #[serde(deserialize_with = "list")]
    pub sets: Vec<Set>,
    #[serde(deserialize_with = "list")]
    pub models: Vec<Model>,
    /// The pictures it was described with (fixed at creation).
    #[serde(deserialize_with = "list")]
    pub references: Vec<Reference>,
}

/// A reference picture, as the service keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reference {
    pub image_id: String,
    /// Short-lived, like a view's.
    pub url: String,
}

/// One round of views: the first, a change, or a fresh start.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Set {
    pub id: String,
    /// `generating`, `ready` or `failed`.
    pub status: String,
    pub made_from: MadeFrom,
    #[serde(deserialize_with = "list")]
    pub views: Vec<ViewImage>,
    pub approved: bool,
    /// Model attempts still allowed from it (four in all, failures not counted). Older answers
    /// called it `rerolls_left`.
    #[serde(default, deserialize_with = "maybe_count")]
    #[serde(alias = "rerolls_left")]
    pub attempts_left: Option<u32>,
    pub error: Option<ApiErr>,
}

/// How a set came about.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MadeFrom {
    /// `new`, `change` or `retry`.
    pub kind: String,
    pub from: Option<String>,
    pub instruction: Option<String>,
}

/// One view of a set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewImage {
    pub label: String,
    pub image_id: String,
    /// Short-lived: every `GET` of the asset gives a fresh one.
    pub url: String,
    #[serde(deserialize_with = "count")]
    pub width: u32,
    #[serde(deserialize_with = "count")]
    pub height: u32,
}

/// One model attempt from an approved set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Model {
    pub id: String,
    /// The live API calls these `asset_id` and `set_id`.
    #[serde(alias = "asset_id")]
    pub asset: String,
    #[serde(alias = "set_id")]
    pub set: String,
    pub engine: String,
    /// `queued`, `running`, `succeeded` or `failed`.
    pub status: String,
    pub progress: Option<f64>,
    pub preview: Option<Preview>,
    pub imported: bool,
    #[serde(default, deserialize_with = "maybe_count")]
    #[serde(alias = "rerolls_left")]
    pub attempts_left: Option<u32>,
    pub created_at: String,
    pub error: Option<ApiErr>,
    /// The mesh to look at (short-lived, fresh on every `GET` of the asset).
    pub preview_url: Option<String>,
    /// The importable file, once imported.
    pub glb_url: Option<String>,
    /// The views it was made from (image ids of the set's views), in the order they went.
    #[serde(deserialize_with = "list")]
    pub input_image_ids: Vec<String>,
    /// Model credits Import spends, and what it did spend.
    #[serde(default, deserialize_with = "maybe_count")]
    pub credits: Option<u32>,
    #[serde(default, deserialize_with = "maybe_count")]
    pub credits_charged: Option<u32>,
    /// The detail it was made at (a [`Detail`] id), its triangle limit, and its finish.
    pub detail: Option<String>,
    #[serde(default, deserialize_with = "maybe_count")]
    pub face_limit: Option<u32>,
    pub finish: Option<String>,
    /// Why it's waiting, while the service says ("Starting the GPU: …"): shown while queued.
    pub note: Option<String>,
    /// The step it's on while queued or running (`paint_back`), and its words ("Painting the
    /// back"); both null once it has finished. Shown as they come: steps may be added.
    pub stage: Option<String>,
    pub stage_label: Option<String>,
}

/// What a finished model can be looked at with: never the importable file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preview {
    pub mesh: Option<PreviewMesh>,
    pub turntable: Option<Turntable>,
    #[serde(deserialize_with = "list")]
    pub stills: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PreviewMesh {
    pub url: String,
    pub reduced: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Turntable {
    pub url: String,
    #[serde(deserialize_with = "count")]
    pub frames: u32,
}

/// What Import (or a finished rig or sound) hands over.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Delivered {
    pub id: String,
    pub imported: bool,
    pub status: String,
    pub progress: Option<f64>,
    #[serde(deserialize_with = "list")]
    pub files: Vec<DeliveredFile>,
    #[serde(deserialize_with = "list")]
    pub animations: Vec<String>,
    #[serde(default, deserialize_with = "maybe_count")]
    pub model_credits_left: Option<u32>,
    pub error: Option<ApiErr>,
    /// Import's answer: the model file.
    pub glb_url: Option<String>,
}

impl Delivered {
    /// The model file's link: `glb_url`, else the first model in `files`.
    pub fn model_url(&self) -> Option<&str> {
        self.glb_url.as_deref().filter(|u| !u.is_empty()).or_else(|| self.files.iter().find(|f| f.kind == "model" || f.format == "glb").map(|f| f.url.as_str()))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeliveredFile {
    /// `model`, `texture`, `sound`.
    pub kind: String,
    pub format: String,
    pub url: String,
}

/// An error as a job or set reports it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiErr {
    pub code: String,
    pub message: String,
}

impl Set {
    pub fn is_generating(&self) -> bool {
        self.status == "generating"
    }

    pub fn is_ready(&self) -> bool {
        self.status == "ready"
    }
}

impl Model {
    /// The mesh to look at: `preview_url`, else the older `preview.mesh`.
    pub fn preview_link(&self) -> Option<&str> {
        self.preview_url.as_deref().filter(|u| !u.is_empty()).or_else(|| self.preview.as_ref().and_then(|p| p.mesh.as_ref()).map(|m| m.url.as_str()))
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running")
    }

    pub fn succeeded(&self) -> bool {
        self.status == "succeeded"
    }

    /// What it's doing now, in the service's words, while it's being made.
    pub fn stage_text(&self) -> Option<&str> {
        self.stage_label.as_deref().map(str::trim).filter(|l| !l.is_empty() && self.is_running())
    }
}

impl Asset {
    /// Its models oldest first, whatever order the service lists them in (the live API lists
    /// the newest first): the last is the latest attempt.
    pub fn in_order(mut self) -> Self {
        self.models.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        self
    }

    /// The set models are made from, if one is approved.
    pub fn approved_set(&self) -> Option<&Set> {
        self.sets.iter().find(|s| s.approved)
    }

    /// The latest set (the one on screen).
    pub fn latest_set(&self) -> Option<&Set> {
        self.sets.last()
    }

    /// Whether anything is still being made (so the card keeps polling).
    pub fn busy(&self) -> bool {
        self.sets.iter().any(Set::is_generating) || self.models.iter().any(Model::is_running)
    }
}

// ----- the asset's folder in the project -----

/// A hosted asset's record in its folder (`assets/<kind>/<name>/hosted.json`): which asset on
/// the service it is, where it was started, and what was saved here. The service's own record
/// (its sets and models) is fetched again when the card opens; what's in the project is what
/// was saved.
pub const RECORD: &str = "hosted.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    /// The asset's id on the service.
    pub id: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub fields: BTreeMap<String, Value>,
    /// The chat it was started in, to put its card back there.
    pub origin: Option<crate::assets::Origin>,
    /// The views saved for each set, by set id: files in the folder.
    pub views: BTreeMap<String, Vec<String>>,
    /// The model attempt imported, and the files it brought.
    pub imported: Option<String>,
    #[serde(deserialize_with = "list")]
    pub files: Vec<String>,
    /// A rig being made (its id), and the rigged files once done.
    pub rig: Option<String>,
    #[serde(deserialize_with = "list")]
    pub rigged: Vec<String>,
    /// The reference pictures it was described with: files in the folder.
    #[serde(deserialize_with = "list")]
    pub references: Vec<String>,
    /// Folded to one line in the chat (`None`: by what it's at; see [`Record::folded`]).
    pub collapsed: Option<bool>,
    /// The person stopped waiting on it: the app no longer asks the service about it (what was
    /// started there still finishes; the service has no way to stop a job yet).
    pub cancelled: bool,
}

impl Record {
    /// Whether its card is folded to one line: as the person left it, else once it's done
    /// (imported) or cancelled.
    pub fn folded(&self) -> bool {
        self.collapsed.unwrap_or(self.cancelled || self.imported.is_some())
    }
}

/// At most this many reference pictures go with a new asset, each at most [`REFERENCE_BYTES`].
pub const REFERENCES: usize = 3;
pub const REFERENCE_BYTES: u64 = 10 * 1024 * 1024;

/// A picture as a reference for a new asset: its bytes in base64, if it is a PNG, JPEG or WebP
/// of at most 10 MB.
pub fn reference(path: &std::path::Path) -> Result<String, String> {
    use base64::Engine as _;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp") {
        return Err(format!("{name}: references are PNG, JPEG or WebP pictures."));
    }
    let size = std::fs::metadata(path).map_err(|e| format!("{name}: {e}"))?.len();
    if size > REFERENCE_BYTES {
        return Err(format!("{name} is over 10 MB; use a smaller picture."));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// The record in `dir`, if it is a hosted asset's folder.
pub fn load(dir: &std::path::Path) -> Option<Record> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(RECORD)).ok()?).ok()
}

/// Write the record in `dir`.
pub fn save(dir: &std::path::Path, record: &Record) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(RECORD), serde_json::to_string_pretty(record).unwrap_or_default())
}

/// A file name for `label` (a view's "Front"): lowercase, safe, with `ext`.
pub fn file_name(label: &str, ext: &str) -> String {
    let base: String = label.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let base = base.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    format!("{}.{ext}", if base.is_empty() { "file" } else { &base })
}

/// Save `url` (one of the service's short-lived links: signed, so no key is sent) to `path`.
pub fn download(url: &str, path: &std::path::Path) -> Result<(), String> {
    if !url.starts_with("https://") && !url.starts_with("http://127.0.0.1") && !url.starts_with("http://localhost") {
        return Err(format!("not a link Wundur downloads: {url}"));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(120))).build().into();
    let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let bytes = resp.body_mut().with_config().limit(512 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

// ----- calls -----

/// How long a call may take: the server waits out its providers' rate limits itself (about
/// 90 seconds before it answers `busy`), so slow answers are normal.
const WAIT: Duration = Duration::from_secs(200);

/// What an answer the client couldn't read is called (the server answered: not a network
/// failure, and worded as such).
const UNREADABLE: &str = "an answer that couldn't be read";

/// A fresh key for a call that spends (see [`account::Account::request`]).
pub fn idempotency_key() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn call<T: serde::de::DeserializeOwned>(method: &str, path: &str, body: Option<&Value>, spends: bool) -> Result<T, Error> {
    let key = spends.then(idempotency_key);
    let text = account::shared().request(Auth::Key, method, path, body, WAIT, key.as_deref())?;
    serde_json::from_str(&text).map_err(|e| Error::Network(format!("{UNREADABLE} ({e})")))
}

/// `GET /v1/catalog`.
pub fn catalog() -> Result<Catalog, Error> {
    call("GET", "/v1/catalog", None, false)
}

/// `POST /v1/assets`: a new asset, with up to three reference pictures (base64, see
/// [`reference`]); its first set is being made (one image spent).
/// Without a `name`, the service names it from the description (and says so in the answer).
pub fn create(kind: &str, name: Option<&str>, description: &str, fields: &BTreeMap<String, Value>, references: &[String]) -> Result<Asset, Error> {
    let mut body = json!({ "kind": kind, "description": description, "fields": fields });
    if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
        body["name"] = json!(name);
    }
    if !references.is_empty() {
        body["references"] = json!(references);
    }
    call::<Asset>("POST", "/v1/assets", Some(&body), true).map(Asset::in_order)
}

/// `GET /v1/assets/{id}`: its state now, with fresh links.
pub fn get(id: &str) -> Result<Asset, Error> {
    call::<Asset>("GET", &format!("/v1/assets/{id}"), None, false).map(Asset::in_order)
}

/// `PATCH /v1/assets/{id}`: a new name, description or fields, used from the next set.
pub fn edit(id: &str, name: Option<&str>, description: Option<&str>, fields: Option<&BTreeMap<String, Value>>) -> Result<Asset, Error> {
    let mut body = serde_json::Map::new();
    if let Some(n) = name {
        body.insert("name".into(), json!(n));
    }
    if let Some(d) = description {
        body.insert("description".into(), json!(d));
    }
    if let Some(f) = fields {
        body.insert("fields".into(), json!(f));
    }
    call::<Asset>("PATCH", &format!("/v1/assets/{id}"), Some(&Value::Object(body)), false).map(Asset::in_order)
}

/// `POST /v1/assets/{id}/sets`: a new set from `from`, changed by `instruction` (the whole set
/// changes), or a fresh start from the description when `instruction` is `None`. One image.
pub fn new_set(id: &str, from: &str, instruction: Option<&str>) -> Result<Asset, Error> {
    let body = match instruction {
        Some(i) => json!({ "from": from, "instruction": i }),
        None => json!({ "from": from, "retry": true }),
    };
    call::<Asset>("POST", &format!("/v1/assets/{id}/sets"), Some(&body), true).map(Asset::in_order)
}

/// `POST /v1/assets/{id}/sets/{set}/approve`: the set models are made from.
pub fn approve(id: &str, set: &str) -> Result<Asset, Error> {
    call::<Asset>("POST", &format!("/v1/assets/{id}/sets/{set}/approve"), None, false).map(Asset::in_order)
}

/// `POST /v1/assets/{id}/models`: a model attempt from the approved set (the server picks the
/// views) on `engine`, or the kind's default. Free; the set's `attempts_left` counts them.
/// The answer isn't relied on: the asset's `models` shows it on the next poll.
/// `detail` is a [`Detail`] id, `finish` one of the engine's finishes (only for an engine that
/// has any); either left out takes the service's default.
pub fn start_model(id: &str, engine: Option<&str>, detail: Option<&str>, finish: Option<&str>) -> Result<(), Error> {
    call::<Value>("POST", &format!("/v1/assets/{id}/models"), Some(&model_body(engine, detail, finish)), true).map(|_| ())
}

/// The body of [`start_model`]: only what was picked.
fn model_body(engine: Option<&str>, detail: Option<&str>, finish: Option<&str>) -> Value {
    let mut body = serde_json::Map::new();
    for (key, value) in [("engine", engine), ("detail", detail), ("finish", finish)] {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            body.insert(key.into(), json!(v));
        }
    }
    Value::Object(body)
}

/// `POST /v1/assets/{id}/models/{model}/import`: spends the engine's credits (once; a repeat is
/// safe) and hands over the file.
pub fn import(id: &str, model: &str) -> Result<Delivered, Error> {
    call("POST", &format!("/v1/assets/{id}/models/{model}/import"), None, true)
}

/// `POST /v1/assets/{id}/models/{model}/reject`: not wanted (optional).
pub fn reject(id: &str, model: &str) -> Result<(), Error> {
    call::<Value>("POST", &format!("/v1/assets/{id}/models/{model}/reject"), None, false).map(|_| ())
}

/// `POST /v1/models/{id}/rig`: rig an imported model (credits spent when it succeeds).
pub fn rig(model: &str, rigger: &str) -> Result<Delivered, Error> {
    call("POST", &format!("/v1/models/{model}/rig"), Some(&json!({ "rigger": rigger })), true)
}

/// `GET /v1/rigs/{id}`.
pub fn get_rig(id: &str) -> Result<Delivered, Error> {
    call("GET", &format!("/v1/rigs/{id}"), None, false)
}

/// `POST /v1/sounds`: `variants` sounds of `seconds` each (all charged).
pub fn sound(engine: &str, prompt: &str, seconds: f64, variants: u32) -> Result<Delivered, Error> {
    call("POST", "/v1/sounds", Some(&json!({ "engine": engine, "prompt": prompt, "seconds": seconds, "variants": variants })), true)
}

/// `GET /v1/sounds/{id}`.
pub fn get_sound(id: &str) -> Result<Delivered, Error> {
    call("GET", &format!("/v1/sounds/{id}"), None, false)
}

/// An error from any of these, worded for the person, and whether Upgrade is the way on.
pub fn explain(e: &Error) -> (String, bool) {
    let Error::Api(a) = e else {
        return match e {
            Error::SignedOut => ("Sign in to your Wundur account (Settings > Account).".into(), false),
            Error::Network(why) if why.starts_with(UNREADABLE) => (
                format!("Wundur's server sent an answer this version can't read{}. Updating Wundur Studio usually fixes it.", why.strip_prefix(UNREADABLE).unwrap_or_default()),
                false,
            ),
            other => (format!("Couldn't reach Wundur's server: {other}"), false),
        };
    };
    let text = match a.code.as_str() {
        "model_credits_used" => "You're out of model credits this month.",
        "image_allowance_used" => "This month's images are used up.",
        "sound_seconds_used" => "This month's sound effects are used up.",
        // The server's words ("{engine} comes with beta access: ask for it in the app.").
        "engine_not_on_plan" | "checkout_closed" if !a.message.is_empty() => "",
        "engine_not_on_plan" => "That engine isn't on your plan.",
        "checkout_closed" => "Wundur Studio is in invite-only beta: ask for access.",
        "attempts_used" | "rerolls_used" => "This set's attempts are used: make a change or a new set of views.",
        "no_approved_set" => "Approve a set of views first.",
        "set_in_progress" => "A set of views is still being made: wait for it to finish.",
        "hourly_limit" | "generation_limit" => "That's a lot of tries for now; try again later.",
        "job_in_progress" => "A model is already being made: wait for it to finish.",
        "content_blocked" => "That description was refused; try rewording it.",
        "provider_error" => "Something went wrong; it wasn't counted. Try again.",
        "busy" => "Busy right now; try again shortly.",
        "invalid_field" | "missing_field" | "unknown_field" | "invalid_request" => "",
        "bad_image" => "A reference picture couldn't be read: use a PNG, JPEG or WebP.",
        "image_too_large" => "A reference picture is too large (10 MB at most).",
        "missing_key" | "invalid_key" | "invalid_token" => "Your Wundur account's key no longer works: sign in again in Settings > Account.",
        "not_found" => "This asset is gone from Wundur's server (they're kept 30 days); its files in the project stay.",
        _ => "",
    };
    let text = match (a.code.as_str(), a.retry_after) {
        ("busy", Some(s)) => format!("Busy right now; try again in {s} s."),
        // A route the server doesn't have (its own errors always carry a code).
        ("http_404" | "http_405", _) => "Wundur's server doesn't offer this step yet; nothing was spent.".to_string(),
        _ if text.is_empty() => if a.message.is_empty() { a.to_string() } else { a.message.clone() },
        _ => text.to_string(),
    };
    (text, a.is_used_up() || needs_access(e))
}

/// The way on from `e` is asking for beta access (an engine not on the plan, checkout closed).
pub fn needs_access(e: &Error) -> bool {
    matches!(e.code(), Some("engine_not_on_plan" | "checkout_closed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::ApiError;

    /// The catalog as the proposal shows it.
    const CATALOG: &str = r#"{
      "version": "2026-11-15.1",
      "kinds": [
        { "id": "humanoid", "label": "Humanoid", "description": "People and anything standing on two legs.",
          "fields": [
            { "id": "style", "label": "Style", "type": "enum", "options": ["realistic", "stylized"], "default": "stylized" },
            { "id": "notes", "label": "Anything else", "type": "text", "required": false }
          ],
          "views": 3, "supports_3d": true, "engines": ["trellis2", "tripo_hd"], "default_engine": "trellis2" },
        { "id": "place", "label": "Place", "fields": [], "views": 1, "supports_3d": false, "engines": [], "offers": ["concept_art"] }
      ],
      "engines": [
        { "id": "trellis2", "label": "Standard", "credits": 1, "plans": ["free", "comp", "monthly"], "seconds": 60, "best_for": ["props"], "help": "Fast.", "license": { "commercial": true, "label": "" }, "multiview": false },
        { "id": "tripo_hd", "label": "High detail", "credits": 3, "plans": ["monthly"], "seconds": 180, "help": "Finer.", "license": { "commercial": true, "label": "" }, "multiview": true }
      ],
      "riggers": [ { "id": "auto", "label": "Automatic rig", "credits": 1, "kinds": ["humanoid"], "seconds": 240 } ],
      "sounds": [ { "id": "sfx", "label": "Sound effect", "max_seconds": 30, "license": { "commercial": true, "label": "" } } ],
      "something_new": { "the app": "ignores it" }
    }"#;

    #[test]
    fn the_catalog_reads_and_answers_what_a_kind_offers() {
        let c: Catalog = serde_json::from_str(CATALOG).unwrap();
        let humanoid = c.kind("humanoid").unwrap();
        assert_eq!((humanoid.views, humanoid.fields.len(), humanoid.fields[0].kind.as_str()), (3, 2, "enum"));
        let engines: Vec<&str> = c.engines_for(humanoid).iter().map(|e| e.id.as_str()).collect();
        assert_eq!(engines, ["trellis2", "tripo_hd"]);
        assert!(!c.engine("tripo_hd").unwrap().on_plan("free"));
        assert!(c.engine("trellis2").unwrap().on_plan("free"));
        assert!(c.engine("tripo_hd").unwrap().multiview && !c.engine("trellis2").unwrap().multiview);
        let place = c.kind("place").unwrap();
        assert!(!place.supports_3d && c.engines_for(place).is_empty() && place.offers == ["concept_art"]);
        assert_eq!(c.riggers_for("humanoid").len(), 1);
        assert!(c.riggers_for("prop").is_empty());
        assert_eq!(c.sounds[0].max_seconds, 30);
    }

    /// The catalog as of the November 2026 update: detail levels, engine finishes, style
    /// pictures.
    const CATALOG_NOV: &str = r#"{
      "version": "2026-11-18.1",
      "kinds": [
        { "id": "humanoid", "label": "Humanoid", "fields": [
            { "id": "style", "label": "Style", "type": "enum", "options": ["stylized", "realistic", "low poly", "toon"], "default": "stylized",
              "help": "Sets the look of the drawings, and the model's paint on Wundur.",
              "option_images": { "low poly": "/v1/catalog/styles/low-poly.jpg", "toon": "/v1/catalog/styles/toon.jpg" } },
            { "id": "mouth", "label": "Mouth", "type": "enum", "options": ["closed", "open"], "default": "closed", "help": "Closed rigs and animates best." }
          ], "views": 3, "supports_3d": true, "engines": ["trellis2", "wundur"], "default_engine": "trellis2" }
      ],
      "engines": [
        { "id": "trellis2", "label": "Standard", "credits": 1, "finishes": [] },
        { "id": "wundur", "label": "Wundur (preview)", "credits": 1, "multiview": true, "finishes": ["smooth", "faceted"],
          "license": { "commercial": false, "label": "Preview: not yet cleared for commercial use" } }
      ],
      "details": [
        { "id": "light", "label": "Light", "triangles": 8000, "help": "Phones and crowds." },
        { "id": "standard", "label": "Standard", "triangles": 30000, "help": "Most game characters." },
        { "id": "high", "label": "High", "triangles": 100000, "help": "Heroes and close-ups." }
      ],
      "default_detail": "standard",
      "faceted_detail": "light"
    }"#;

    #[test]
    fn the_november_catalog_reads_details_finishes_and_style_pictures() {
        let c: Catalog = serde_json::from_str(CATALOG_NOV).unwrap();
        let style = &c.kind("humanoid").unwrap().fields[0];
        assert_eq!(style.option_images.get("low poly").map(String::as_str), Some("/v1/catalog/styles/low-poly.jpg"));
        assert!(!style.option_images.contains_key("realistic"), "an option without a picture");
        assert_eq!(c.details.iter().map(|d| (d.id.as_str(), d.triangles)).collect::<Vec<_>>(), [("light", 8000), ("standard", 30000), ("high", 100000)]);
        assert_eq!(c.detail_for(false).map(|d| d.id.as_str()), Some("standard"));
        assert_eq!(c.detail_for(true).map(|d| d.id.as_str()), Some("light"));
        let (wundur, standard) = (c.engine("wundur").unwrap(), c.engine("trellis2").unwrap());
        assert_eq!(default_finish(wundur, true).as_deref(), Some("faceted"));
        assert_eq!(default_finish(wundur, false).as_deref(), Some("smooth"));
        assert_eq!(default_finish(standard, true), None, "no finishes: none sent");
        assert!(low_poly(Some("low poly")) && !low_poly(Some("toon")) && !low_poly(None));
        // An older catalog, without any of it: nothing to pick, nothing breaks.
        let old: Catalog = serde_json::from_str(CATALOG).unwrap();
        assert!(old.details.is_empty() && old.detail_for(false).is_none());
        assert!(old.engines.iter().all(|e| e.finishes.is_empty()));
    }

    #[test]
    fn a_model_says_its_detail_finish_and_why_it_waits() {
        let m: Model = serde_json::from_str(r#"{ "id": "m1", "engine": "wundur", "status": "queued", "detail": "light", "face_limit": 8000, "finish": "faceted",
            "note": "Starting the GPU: the first model after a quiet spell takes about 10 minutes." }"#).unwrap();
        assert_eq!((m.detail.as_deref(), m.face_limit, m.finish.as_deref()), (Some("light"), Some(8000), Some("faceted")));
        assert!(m.note.unwrap().starts_with("Starting the GPU"));
    }

    #[test]
    fn a_model_is_asked_for_with_only_what_was_picked() {
        assert_eq!(model_body(Some("wundur"), Some("light"), Some("faceted")), json!({ "engine": "wundur", "detail": "light", "finish": "faceted" }));
        assert_eq!(model_body(Some("trellis2"), Some("standard"), None), json!({ "engine": "trellis2", "detail": "standard" }));
        assert_eq!(model_body(None, None, Some("")), json!({}));
    }

    #[test]
    fn the_settings_line_says_what_each_is_set_to() {
        let field = |id: &str, label: &str| Field { id: id.into(), label: label.into(), kind: "enum".into(), ..Default::default() };
        let humanoid = [field("style", "Style"), field("height", "Height"), field("hands", "Hands"), field("feet", "Feet"), field("mouth", "Mouth"), Field { id: "notes".into(), label: "Anything else".into(), kind: "text".into(), ..Default::default() }];
        let pick = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>();
        let none: BTreeMap<String, String> = BTreeMap::new();
        let off: BTreeMap<String, bool> = BTreeMap::new();
        let defaults = pick(&[("style", "low poly"), ("height", "adult"), ("hands", "auto"), ("feet", "auto"), ("mouth", "closed")]);
        assert_eq!(settings_summary(&humanoid, &defaults, &off), "Adult height · Auto hands & feet · Closed mouth");
        let changed = pick(&[("height", "giant"), ("hands", "human-like"), ("feet", "as described"), ("mouth", "open")]);
        assert_eq!(settings_summary(&humanoid, &changed, &off), "Giant height · Human-like hands · Feet as described · Open mouth");
        let creature = [field("legs", "Stands on"), field("hands", "Hands")];
        assert_eq!(settings_summary(&creature, &pick(&[("legs", "four legs"), ("hands", "auto")]), &off), "Stands on four legs · Auto hands");
        let switch = [Field { id: "tail".into(), label: "Tail".into(), kind: "bool".into(), ..Default::default() }];
        assert_eq!(settings_summary(&switch, &none, &[("tail".to_string(), true)].into_iter().collect()), "Tail");
        assert_eq!(settings_summary(&[], &none, &off), "");
    }

    #[test]
    fn the_form_offers_stylized_and_realistic_only() {
        let f = Field { id: "style".into(), kind: "enum".into(), options: vec!["stylized".into(), "realistic".into(), "low poly".into(), "toon".into()], ..Default::default() };
        assert_eq!(looks(&f), vec!["stylized".to_string(), "realistic".to_string()]);
        let other = Field { options: vec!["ink".into(), "clay".into()], ..f };
        assert_eq!(looks(&other), vec!["ink".to_string(), "clay".to_string()], "a catalog with neither keeps its own");
    }

    #[test]
    fn a_card_folds_once_done_or_cancelled_unless_opened() {
        let mut r = Record::default();
        assert!(!r.folded());
        r.imported = Some("md_1".into());
        assert!(r.folded());
        r.collapsed = Some(false);
        assert!(!r.folded(), "opened by the person");
        let r = Record { cancelled: true, ..Record::default() };
        assert!(r.folded());
    }

    #[test]
    fn a_name_is_suggested_from_the_description() {
        assert_eq!(suggested_name("A tiger raptor scout wearing a vest"), "Tiger raptor scout");
        assert_eq!(suggested_name("a wiry goblin scout with olive-green skin, a hood"), "Wiry goblin scout");
        assert_eq!(suggested_name("Treasure chest. Iron bands."), "Treasure chest");
        assert_eq!(suggested_name("an enormous ancient stone golem guardian of the gate"), "Enormous ancient stone golem guardian");
        assert_eq!(suggested_name("   "), "");
    }

    #[test]
    fn an_asset_reads_with_its_sets_and_models() {
        let a: Asset = serde_json::from_str(
            r#"{ "id": "as_7f2", "kind": "humanoid", "name": "Goblin scout", "fields": { "style": "stylized" },
                 "sets": [
                   { "id": "set_1", "status": "ready", "made_from": { "kind": "new" }, "approved": false, "rerolls_left": 4,
                     "views": [ { "label": "Front", "image_id": "img_a", "url": "https://x/a", "width": 512, "height": 1024 } ] },
                   { "id": "set_2", "status": "generating", "made_from": { "kind": "change", "from": "set_1", "instruction": "darker" } }
                 ],
                 "models": [ { "id": "md_1", "set": "set_1", "engine": "trellis2", "status": "running", "progress": 0.4 },
                             { "id": "job_2", "set_id": "set_1", "asset_id": "as_1", "engine": "tripo", "status": "succeeded", "preview_url": "https://x/p.glb", "input_image_ids": ["img_f", "img_s", "img_b"] } ] }"#,
        )
        .unwrap();
        assert_eq!(a.sets[0].attempts_left, Some(4), "the review's name still reads");
        assert_eq!(a.sets[0].views[0].width, 512);
        assert!(a.busy());
        assert_eq!(a.latest_set().unwrap().made_from.instruction.as_deref(), Some("darker"));
        assert!(a.approved_set().is_none());
        assert_eq!((a.models[1].set.as_str(), a.models[1].asset.as_str()), ("set_1", "as_1"), "the live API's set_id and asset_id");
        assert_eq!(a.models[1].preview_link(), Some("https://x/p.glb"));
        assert_eq!(a.models[1].input_image_ids, ["img_f", "img_s", "img_b"]);
        let older: Model = serde_json::from_str(r#"{ "id": "job_1", "input_image_ids": null, "error": null, "glb_url": null }"#).unwrap();
        assert!(older.input_image_ids.is_empty(), "an older model's null list reads as empty");
        let mut newest_first = a.clone();
        newest_first.models[0].created_at = "2026-11-16T13:22:06+00:00".into();
        newest_first.models[1].created_at = "2026-11-16T12:57:07+00:00".into();
        assert_eq!(newest_first.in_order().models.last().map(|m| m.id.as_str()), Some("md_1"), "the latest attempt is last");
    }

    #[test]
    fn a_preview_and_an_import_read() {
        let m: Model = serde_json::from_str(
            r#"{ "id": "md_9", "status": "succeeded", "attempts_left": 2, "preview": { "mesh": { "url": "https://x/p.glb", "reduced": true } } }"#,
        )
        .unwrap();
        assert!(m.succeeded() && !m.is_running());
        assert!(m.preview.unwrap().mesh.unwrap().reduced);
        let d: Delivered = serde_json::from_str(
            r#"{ "id": "md_9", "imported": true, "files": [ { "kind": "model", "format": "glb", "url": "https://x/m.glb" } ], "model_credits_left": 1 }"#,
        )
        .unwrap();
        assert_eq!((d.files[0].format.as_str(), d.model_credits_left), ("glb", Some(1)));
    }

    #[test]
    fn numbers_read_whole_decimal_or_text() {
        let e: Engine = serde_json::from_str(r#"{ "id": "x", "credits": 2.0, "seconds": "60" }"#).unwrap();
        assert_eq!((e.credits, e.seconds), (2, 60));
        let s: SoundEngine = serde_json::from_str(r#"{ "id": "sfx", "max_seconds": 30.0 }"#).unwrap();
        assert_eq!(s.max_seconds, 30);
        let set: Set = serde_json::from_str(r#"{ "id": "s", "attempts_left": null }"#).unwrap();
        assert_eq!(set.attempts_left, None);
        let set: Set = serde_json::from_str(r#"{ "id": "s", "rerolls_left": 3.0 }"#).unwrap();
        assert_eq!(set.attempts_left, Some(3));
    }

    #[test]
    fn the_catalog_says_which_engines_this_account_may_use() {
        let c: Catalog = serde_json::from_str(
            r#"{"engines": [{ "id": "wundur", "available": true }, { "id": "tripo", "plans": ["free"], "available": false }, { "id": "old", "plans": ["monthly"] }]}"#,
        )
        .unwrap();
        assert!(c.engine("wundur").unwrap().on_plan("free"));
        assert!(!c.engine("tripo").unwrap().on_plan("free"), "available wins over plans");
        assert!(!c.engine("old").unwrap().on_plan("free") && c.engine("old").unwrap().on_plan("monthly"), "an older catalog: by plan");
    }

    #[test]
    fn errors_say_what_to_do() {
        let api = |status: u16, code: &str| Error::Api(ApiError { status, code: code.into(), message: format!("{code} said"), retry_after: None });
        let (text, upgrade) = explain(&api(402, "model_credits_used"));
        assert!(text.contains("out of model credits") && upgrade);
        let (_, upgrade) = explain(&api(429, "image_allowance_used"));
        assert!(upgrade, "an older API's 429 for an allowance still offers Upgrade");
        let (text, upgrade) = explain(&api(429, "hourly_limit"));
        assert!(text.contains("try again later") && !upgrade);
        let busy = Error::Api(ApiError { status: 503, code: "busy".into(), message: String::new(), retry_after: Some(34) });
        assert_eq!(explain(&busy).0, "Busy right now; try again in 34 s.");
        assert!(explain(&api(404, "http_404")).0.contains("nothing was spent"), "a route the server lacks");
        assert!(explain(&api(422, "content_blocked")).0.contains("rewording"));
        assert!(explain(&api(400, "bad_image")).0.contains("PNG, JPEG or WebP"));
        assert!(explain(&api(413, "image_too_large")).0.contains("10 MB"));
        assert_eq!(explain(&api(422, "invalid_request")).0, "invalid_request said", "the server's own words for a bad request");
        assert!(explain(&api(403, "engine_not_on_plan")).1);
        assert_eq!(explain(&api(403, "engine_not_on_plan")).0, "engine_not_on_plan said", "the server's words for a locked engine");
        assert!(needs_access(&api(403, "engine_not_on_plan")) && needs_access(&api(403, "checkout_closed")));
        assert!(!needs_access(&api(402, "model_credits_used")));
        let bare = Error::Api(ApiError { status: 403, code: "checkout_closed".into(), message: String::new(), retry_after: None });
        assert!(explain(&bare).0.contains("ask for access"));
        assert_eq!(explain(&api(500, "weird")).0, "weird said");
        assert!(explain(&Error::SignedOut).0.contains("Sign in"));
    }

    #[test]
    fn a_record_round_trips_and_names_are_safe() {
        let dir = std::env::temp_dir().join(format!("wundur-flow-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = Record { id: "as_1".into(), kind: "humanoid".into(), name: "Goblin".into(), ..Record::default() };
        r.views.insert("set_1".into(), vec!["views/set_1/front.png".into()]);
        save(&dir, &r).unwrap();
        assert_eq!(load(&dir), Some(r));
        assert!(load(&dir.join("nope")).is_none());
        assert_eq!(file_name("Front 3/4", "png"), "front-3-4.png");
        assert_eq!(file_name("", "glb"), "file.glb");
        assert!(download("file:///etc/passwd", &dir.join("x")).is_err(), "only web links");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The live catalog and account, read with this machine's key (read-only, nothing spent):
    /// `cargo test -p wundur-core live_catalog -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_live_catalog_and_account_read() {
        let c = catalog().unwrap();
        eprintln!("catalog {}: kinds {:?}", c.version, c.kinds.iter().map(|k| (&k.id, k.views, k.supports_3d, &k.engines, k.fields.len())).collect::<Vec<_>>());
        eprintln!("engines {:?}", c.engines.iter().map(|e| (&e.id, e.credits, &e.plans, e.seconds)).collect::<Vec<_>>());
        eprintln!("sounds {:?}, riggers {}", c.sounds.iter().map(|s| (&s.id, s.max_seconds)).collect::<Vec<_>>(), c.riggers.len());
        for k in &c.kinds {
            for f in &k.fields {
                eprintln!("{}.{} {:?} help {:?} option_help {:?} images {}", k.id, f.id, f.options, f.help, f.option_help, f.option_images.len());
            }
        }
        assert!(!c.kinds.is_empty() && !c.engines.is_empty());
        for k in c.kinds.iter().filter(|k| k.supports_3d) {
            assert!(k.engines.iter().all(|e| c.engine(e).is_some()), "{}'s engines are in the catalog", k.id);
            assert!(c.engine(&k.default_engine).is_some(), "{}'s default engine", k.id);
        }
        let me = crate::account::shared().me_with(Auth::Key).unwrap();
        eprintln!("plan {} model_credits {:?} sound_seconds {:?} images {:?}", me.plan, me.allowance.model_credits.map(|u| u.left), me.allowance.sound_seconds.map(|u| u.left), me.allowance.images.left);
        assert!(me.allowance.model_credits.is_some(), "the credit buckets are live");
    }

    #[test]
    fn references_are_pictures_of_at_most_10_mb() {
        let dir = std::env::temp_dir().join(format!("wundur-refs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"\x89PNG").unwrap();
        std::fs::write(dir.join("a.gif"), b"GIF89a").unwrap();
        assert!(reference(&dir.join("a.png")).is_ok());
        assert!(reference(&dir.join("a.gif")).unwrap_err().contains("PNG, JPEG or WebP"));
        assert!(reference(&dir.join("missing.jpg")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn keys_for_spending_calls_are_fresh_each_time() {
        let (a, b) = (idempotency_key(), idempotency_key());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }

    #[test]
    fn a_model_says_what_step_it_is_on_while_being_made() {
        let m: Model = serde_json::from_str(r#"{ "id": "m1", "status": "running", "stage": "paint_back", "stage_label": "Painting the back" }"#).unwrap();
        assert_eq!((m.stage.as_deref(), m.stage_text()), (Some("paint_back"), Some("Painting the back")));
        let done: Model = serde_json::from_str(r#"{ "id": "m1", "status": "succeeded", "stage": null, "stage_label": null }"#).unwrap();
        assert_eq!(done.stage_text(), None);
        let older: Model = serde_json::from_str(r#"{ "id": "m1", "status": "queued" }"#).unwrap();
        assert_eq!(older.stage_text(), None, "an older API without stages still reads");
    }

    #[test]
    fn a_field_explains_the_option_picked_else_itself() {
        let mut f = Field { help: "How tall it stands.".into(), ..Default::default() };
        f.option_help.insert("adult".into(), "Adult proportions.".into());
        assert_eq!(f.help_for("adult"), "Adult proportions.");
        assert_eq!(f.help_for("child"), "How tall it stands.");
    }
}
