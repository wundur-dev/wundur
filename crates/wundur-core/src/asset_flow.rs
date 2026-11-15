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

// ----- the catalog -----

/// What the service offers, from `GET /v1/catalog`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    pub version: String,
    pub kinds: Vec<Kind>,
    pub engines: Vec<Engine>,
    pub riggers: Vec<Rigger>,
    pub sounds: Vec<SoundEngine>,
}

/// A kind of thing to make (a humanoid, a creature, a prop, a place).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Kind {
    pub id: String,
    pub label: String,
    pub description: String,
    /// The form the person fills in, in order.
    pub fields: Vec<Field>,
    /// How many views it usually gets, for placeholders while they're made (the real number
    /// is whatever the set holds).
    pub views: u32,
    pub supports_3d: bool,
    /// The engines that can make it, by id.
    pub engines: Vec<String>,
    pub default_engine: String,
    /// What's offered instead of a model when it has none (`concept_art`).
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
    pub options: Vec<String>,
    pub default: Option<Value>,
    pub required: bool,
    pub help: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
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
    pub credits: u32,
    /// The plans that may use it.
    pub plans: Vec<String>,
    /// About how long a model takes.
    pub seconds: u32,
    pub best_for: Vec<String>,
    pub help: String,
    pub license: License,
}

/// A rigger, for an imported model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rigger {
    pub id: String,
    pub label: String,
    pub credits: u32,
    /// The kinds it rigs.
    pub kinds: Vec<String>,
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
}

impl Engine {
    /// Whether the person's `plan` may use it (an engine listing no plans is open to all).
    pub fn on_plan(&self, plan: &str) -> bool {
        self.plans.is_empty() || self.plans.iter().any(|p| p == plan)
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
    pub sets: Vec<Set>,
    pub models: Vec<Model>,
}

/// One round of views: the first, a change, or a fresh start.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Set {
    pub id: String,
    /// `generating`, `ready` or `failed`.
    pub status: String,
    pub made_from: MadeFrom,
    pub views: Vec<ViewImage>,
    pub approved: bool,
    /// Model attempts still allowed from it (four in all, failures not counted). Older answers
    /// called it `rerolls_left`.
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
    pub width: u32,
    pub height: u32,
}

/// One model attempt from an approved set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Model {
    pub id: String,
    pub asset: String,
    pub set: String,
    pub engine: String,
    /// `queued`, `running`, `succeeded` or `failed`.
    pub status: String,
    pub progress: Option<f64>,
    pub preview: Option<Preview>,
    pub imported: bool,
    #[serde(alias = "rerolls_left")]
    pub attempts_left: Option<u32>,
    pub created_at: String,
    pub error: Option<ApiErr>,
}

/// What a finished model can be looked at with: never the importable file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preview {
    pub mesh: Option<PreviewMesh>,
    pub turntable: Option<Turntable>,
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
    pub files: Vec<DeliveredFile>,
    pub animations: Vec<String>,
    pub model_credits_left: Option<u32>,
    pub error: Option<ApiErr>,
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
    pub fn is_running(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running")
    }

    pub fn succeeded(&self) -> bool {
        self.status == "succeeded"
    }
}

impl Asset {
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

// ----- calls -----

/// How long a call may take: answers are quick (the work happens in jobs).
const WAIT: Duration = Duration::from_secs(30);

/// A fresh key for a call that spends (see [`account::Account::request`]).
pub fn idempotency_key() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn call<T: serde::de::DeserializeOwned>(method: &str, path: &str, body: Option<&Value>, spends: bool) -> Result<T, Error> {
    let key = spends.then(idempotency_key);
    let text = account::shared().request(Auth::Key, method, path, body, WAIT, key.as_deref())?;
    serde_json::from_str(&text).map_err(|e| Error::Network(format!("an answer that couldn't be read ({e})")))
}

/// `GET /v1/catalog`.
pub fn catalog() -> Result<Catalog, Error> {
    call("GET", "/v1/catalog", None, false)
}

/// `POST /v1/assets`: a new asset; its first set is being made (one image spent).
pub fn create(kind: &str, name: &str, description: &str, fields: &BTreeMap<String, Value>) -> Result<Asset, Error> {
    call("POST", "/v1/assets", Some(&json!({ "kind": kind, "name": name, "description": description, "fields": fields })), true)
}

/// `GET /v1/assets/{id}`: its state now, with fresh links.
pub fn get(id: &str) -> Result<Asset, Error> {
    call("GET", &format!("/v1/assets/{id}"), None, false)
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
    call("PATCH", &format!("/v1/assets/{id}"), Some(&Value::Object(body)), false)
}

/// `POST /v1/assets/{id}/sets`: a new set from `from`, changed by `instruction` (the whole set
/// changes), or a fresh start from the description when `instruction` is `None`. One image.
pub fn new_set(id: &str, from: &str, instruction: Option<&str>) -> Result<Asset, Error> {
    let body = match instruction {
        Some(i) => json!({ "from": from, "instruction": i }),
        None => json!({ "from": from, "retry": true }),
    };
    call("POST", &format!("/v1/assets/{id}/sets"), Some(&body), true)
}

/// `POST /v1/assets/{id}/sets/{set}/approve`: the set models are made from.
pub fn approve(id: &str, set: &str) -> Result<Asset, Error> {
    call("POST", &format!("/v1/assets/{id}/sets/{set}/approve"), None, false)
}

/// `POST /v1/assets/{id}/models`: a model attempt from `set` on `engine` (nothing the person
/// sees is spent).
pub fn start_model(id: &str, set: &str, engine: &str) -> Result<Model, Error> {
    call("POST", &format!("/v1/assets/{id}/models"), Some(&json!({ "set": set, "engine": engine })), false)
}

/// `POST /v1/models/{id}/reroll`: another attempt, on `engine` when given.
pub fn reroll(model: &str, engine: Option<&str>) -> Result<Model, Error> {
    let body = engine.map(|e| json!({ "engine": e }));
    call("POST", &format!("/v1/models/{model}/reroll"), body.as_ref(), false)
}

/// `POST /v1/models/{id}/import`: spends the engine's credits and hands over the files.
pub fn import(model: &str) -> Result<Delivered, Error> {
    call("POST", &format!("/v1/models/{model}/import"), None, true)
}

/// `POST /v1/models/{id}/reject`: not wanted (optional).
pub fn reject(model: &str) -> Result<(), Error> {
    call::<Value>("POST", &format!("/v1/models/{model}/reject"), None, false).map(|_| ())
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
            other => (format!("Couldn't reach Wundur's server: {other}"), false),
        };
    };
    let text = match a.code.as_str() {
        "model_credits_used" => "You're out of model credits this month.",
        "image_allowance_used" => "This month's images are used up.",
        "sound_seconds_used" => "This month's sound effects are used up.",
        "engine_not_on_plan" => "That engine is on the Monthly plan.",
        "rerolls_used" => "That's all the tries for these views: change the views to try again.",
        "hourly_limit" => "That's the most for this hour; try again in a while.",
        "generation_limit" => "That's a lot of tries this month; try again tomorrow, or contact us.",
        "job_in_progress" => "A model is already being made: wait for it to finish.",
        "content_blocked" => "The prompt was refused by the content filter; try saying it differently.",
        "provider_error" | "busy" => "Wundur's service is busy; try again in a minute (this one isn't counted).",
        "missing_key" | "invalid_key" | "invalid_token" => "Your Wundur account's key no longer works: sign in again in Settings > Account.",
        "not_found" => "This asset is gone from Wundur's server (they're kept 30 days); its files in the project stay.",
        _ => "",
    };
    let text = if text.is_empty() { if a.message.is_empty() { a.to_string() } else { a.message.clone() } } else { text.to_string() };
    (text, a.is_used_up() || a.code == "engine_not_on_plan")
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
        { "id": "trellis2", "label": "Standard", "credits": 1, "plans": ["free", "comp", "monthly"], "seconds": 60, "best_for": ["props"], "help": "Fast.", "license": { "commercial": true, "label": "" } },
        { "id": "tripo_hd", "label": "High detail", "credits": 3, "plans": ["monthly"], "seconds": 180, "help": "Finer.", "license": { "commercial": true, "label": "" } }
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
        let place = c.kind("place").unwrap();
        assert!(!place.supports_3d && c.engines_for(place).is_empty() && place.offers == ["concept_art"]);
        assert_eq!(c.riggers_for("humanoid").len(), 1);
        assert!(c.riggers_for("prop").is_empty());
        assert_eq!(c.sounds[0].max_seconds, 30);
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
                 "models": [ { "id": "md_1", "set": "set_1", "engine": "trellis2", "status": "running", "progress": 0.4 } ] }"#,
        )
        .unwrap();
        assert_eq!(a.sets[0].attempts_left, Some(4), "the review's name still reads");
        assert_eq!(a.sets[0].views[0].width, 512);
        assert!(a.busy());
        assert_eq!(a.latest_set().unwrap().made_from.instruction.as_deref(), Some("darker"));
        assert!(a.approved_set().is_none());
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
    fn errors_say_what_to_do() {
        let api = |status: u16, code: &str| Error::Api(ApiError { status, code: code.into(), message: format!("{code} said") });
        let (text, upgrade) = explain(&api(402, "model_credits_used"));
        assert!(text.contains("out of model credits") && upgrade);
        let (_, upgrade) = explain(&api(429, "image_allowance_used"));
        assert!(upgrade, "an older API's 429 for an allowance still offers Upgrade");
        let (text, upgrade) = explain(&api(429, "hourly_limit"));
        assert!(text.contains("this hour") && !upgrade);
        assert!(explain(&api(403, "engine_not_on_plan")).1);
        assert_eq!(explain(&api(500, "weird")).0, "weird said");
        assert!(explain(&Error::SignedOut).0.contains("Sign in"));
    }

    #[test]
    fn keys_for_spending_calls_are_fresh_each_time() {
        let (a, b) = (idempotency_key(), idempotency_key());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
