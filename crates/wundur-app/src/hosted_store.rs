//! The hosted assets in this window (see `wundur_core::asset_flow`): each asset's record in its
//! folder, the service's state of it, what is being asked now and what went wrong. The chat's
//! card and the asset's tab draw from here and act through it. Calls run off the UI thread;
//! while anything is being made the store polls the service, saves each finished set of views
//! into the asset's folder, and redraws.
//!
//! Spending is only ever a person's click: an agent can open a card (with a description), and
//! nothing here spends on its own.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::*;
use serde_json::Value;
use wundur_core::account::{self, Auth, Error};
use wundur_core::asset_flow::{self as flow, Asset, Catalog, Record};

/// How often the service is asked while something is being made.
const POLL: Duration = Duration::from_millis(2500);

/// One hosted asset as the window holds it.
#[derive(Clone, Default)]
pub struct Entry {
    pub record: Record,
    /// The project it is in.
    pub project: PathBuf,
    /// The service's state of it, once fetched.
    pub asset: Option<Asset>,
    /// What is being asked of the service now ("Starting", "Importing"), if anything.
    pub working: Option<&'static str>,
    /// What went wrong last, and whether Upgrade is the way on.
    pub error: Option<(String, bool)>,
    /// The engine, detail and finish picked for the next model (else the defaults: see
    /// [`model_choice`]).
    pub engine: Option<String>,
    pub detail: Option<String>,
    pub finish: Option<String>,
    /// Previews downloaded (outside the project), by model attempt.
    pub previews: HashMap<String, PathBuf>,
    /// The step the card shows when the person went back to one (else the asset's own), and
    /// the set of views picked from the versions (else the latest).
    pub step: Option<u8>,
    pub shown_set: Option<String>,
    /// A model was asked for and the service hasn't listed it yet: the models it had then,
    /// and when (the card shows it building meanwhile, and the store keeps asking).
    pub building: Option<(usize, std::time::Instant)>,
    /// Imported while the card was on screen: it stays open until the person folds it (or the
    /// chat is read in again), so the result and the next step are still there.
    pub just_done: bool,
    polling: bool,
}

impl Entry {
    /// Whether its card is folded to one line (see [`flow::Record::folded`]).
    pub fn folded(&self) -> bool {
        match self.record.collapsed {
            Some(c) => c,
            None => !self.just_done && self.record.folded(),
        }
    }

    /// Whether something is being made or asked for that the person is waiting on.
    pub fn busy(&self) -> bool {
        !self.record.cancelled && (self.working.is_some() || self.building.is_some() || self.asset.as_ref().is_some_and(Asset::busy))
    }

    /// Whether Cancel job is offered: something is being made, and it isn't an import (that is
    /// paid for once asked, and brings the files whatever the card does).
    pub fn cancellable(&self) -> bool {
        self.busy() && self.working != Some("Importing")
    }
}

#[derive(Default)]
struct Store {
    entries: HashMap<PathBuf, Entry>,
    catalog: Option<Catalog>,
    /// The person's plan (`free`, `monthly`…), for which engines they may use.
    plan: Option<String>,
    /// When the plan's allowance resets (paid plans: what's said instead of Upgrade).
    period_end: Option<String>,
    /// Images left this month, for the cost line on the form.
    images_left: Option<i64>,
    /// Style pictures being fetched now, by path (so each is asked for once).
    fetching: std::collections::HashSet<String>,
    loading: bool,
    catalog_error: Option<String>,
    revision: u64,
}

impl Global for Store {}

pub fn revision(cx: &App) -> u64 {
    cx.try_global::<Store>().map_or(0, |s| s.revision)
}

/// Whether `dir` is a hosted asset's folder.
pub fn is_hosted(dir: &Path) -> bool {
    dir.join(flow::RECORD).is_file()
}

/// Whether New asset uses the hosted flow: always, unless `WUNDUR_OWN_KEY_ASSETS=1` brings back
/// the older card that calls image and model services with the person's own keys (a dev flag).
pub fn flow_on() -> bool {
    !wundur_core::env_var("OWN_KEY_ASSETS").is_ok_and(|v| v != "0")
}

/// Whether the person is signed in to a Wundur account (asked once, then remembered).
pub fn signed_in() -> bool {
    account::shared().is_signed_in()
}

/// The catalog and the person's plan, fetched the first time (and again after a failure).
pub fn catalog(cx: &mut App) -> (Option<Catalog>, Option<String>) {
    let store = cx.default_global::<Store>();
    if store.catalog.is_none() && !store.loading {
        store.loading = true;
        cx.spawn(async move |cx: &mut AsyncApp| {
            let got = cx.background_spawn(async move { (flow::catalog(), account::shared().me_with(Auth::Key).map(|m| (m.plan, (m.period_end, m.allowance.images.left)))) }).await;
            cx.update(|cx| {
                let store = cx.default_global::<Store>();
                store.loading = false;
                match got {
                    (Ok(c), me) => {
                        store.catalog = Some(c);
                        let (plan, rest) = me.ok().unzip();
                        let (period_end, images_left) = rest.unzip();
                        store.plan = plan;
                        store.period_end = period_end.flatten();
                        store.images_left = images_left;
                        store.catalog_error = None;
                    }
                    (Err(e), _) => store.catalog_error = Some(flow::explain(&e).0),
                }
                store.revision += 1;
                cx.refresh_windows();
            });
        })
        .detach();
    }
    let store = cx.global::<Store>();
    (store.catalog.clone(), store.plan.clone())
}

/// Images left this month (`GET /v1/me`), once known.
pub fn images_left(cx: &App) -> Option<i64> {
    cx.try_global::<Store>().and_then(|s| s.images_left)
}

/// Ask again how many images are left (after one was spent).
fn refresh_allowance(cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx.background_spawn(async move { account::shared().me_with(Auth::Key) }).await;
        cx.update(|cx| {
            if let Ok(me) = got {
                let store = cx.default_global::<Store>();
                store.images_left = Some(me.allowance.images.left);
                store.revision += 1;
                cx.refresh_windows();
            }
        });
    })
    .detach();
}

/// How long a style picture is kept before it's fetched again (the service caches them a day).
const STYLE_IMAGE_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// A style option's picture (`path` from the field's `option_images`, under the API's base
/// URL) as a file on this machine: the one kept from earlier, fetched again once it's a day
/// old. `None` until the first fetch lands (the tile shows its name meanwhile).
pub fn style_image(path: &str, cx: &mut App) -> Option<PathBuf> {
    let name: String = path.trim_start_matches('/').chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let file = dirs::cache_dir()?.join("wundur-studio").join("styles").join(name);
    let fresh = std::fs::metadata(&file).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < STYLE_IMAGE_AGE);
    let have = file.is_file().then(|| file.clone());
    let store = cx.default_global::<Store>();
    if fresh || store.fetching.contains(path) {
        return have;
    }
    store.fetching.insert(path.to_string());
    let (path, into) = (path.to_string(), file);
    let url = format!("{}{}", account::shared().api_base().trim_end_matches('/'), if path.starts_with('/') { path.clone() } else { format!("/{path}") });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx.background_spawn(async move { flow::download(&url, &into) }).await;
        cx.update(|cx| {
            let store = cx.default_global::<Store>();
            store.fetching.remove(&path);
            if got.is_ok() {
                // A new picture under the same file name: drop the drawn one.
                store.revision += 1;
                cx.refresh_windows();
            }
        });
    })
    .detach();
    have
}

/// When the person's monthly allowance resets, on a paid plan (`None` on the free one).
pub fn resets_on(cx: &App) -> Option<String> {
    let store = cx.try_global::<Store>()?;
    if store.plan.as_deref().is_none_or(|p| p == "free") {
        return None;
    }
    store.period_end.as_ref().map(|d| d.get(..10).unwrap_or(d).to_string())
}

/// Why the catalog isn't there, if fetching it failed (`catalog` tries again).
pub fn catalog_error(cx: &App) -> Option<String> {
    cx.try_global::<Store>().and_then(|s| s.catalog_error.clone())
}

/// Forget the catalog and fetch it again (a retry after an error).
pub fn reload_catalog(cx: &mut App) {
    let store = cx.default_global::<Store>();
    store.catalog = None;
    store.catalog_error = None;
    catalog(cx);
}

/// The asset in `dir`, read from its record the first time (and fetched from the service).
pub fn entry(dir: &Path, cx: &mut App) -> Option<Entry> {
    let fresh = {
        let store = cx.default_global::<Store>();
        if store.entries.contains_key(dir) {
            false
        } else {
            let record = flow::load(dir)?;
            store.entries.insert(dir.to_path_buf(), Entry { record, project: crate::asset_store::project_of(dir), ..Default::default() });
            true
        }
    };
    if fresh {
        poll(dir, cx);
    }
    cx.global::<Store>().entries.get(dir).cloned()
}

/// Change the entry for `dir` (saving its record when `save`), and redraw.
fn update(dir: &Path, save: bool, cx: &mut App, f: impl FnOnce(&mut Entry)) {
    let store = cx.default_global::<Store>();
    if let Some(e) = store.entries.get_mut(dir) {
        f(e);
        if save && let Err(err) = flow::save(dir, &e.record) {
            e.error = Some((format!("Couldn't save the asset's record: {err}"), false));
        }
    }
    store.revision += 1;
    cx.refresh_windows();
}

/// Remember the chat the asset in `dir` belongs to, when it doesn't know yet (made before the
/// chat had an id): its card comes back when that chat is read in again. `false` when it
/// already knows, or isn't a hosted asset.
pub fn claim(dir: &Path, origin: &wundur_core::assets::Origin, cx: &mut App) -> bool {
    if entry(dir, cx).is_none_or(|e| e.record.origin.is_some()) {
        return false;
    }
    let store = cx.global_mut::<Store>();
    let Some(e) = store.entries.get_mut(dir) else { return false };
    e.record.origin = Some(origin.clone());
    if let Err(err) = flow::save(dir, &e.record) {
        eprintln!("wundur: remembering {}'s chat: {err}", dir.display());
    }
    true
}

/// Show step `step` (1 describe … 4 rig), or the asset's own with `None`.
pub fn show_step(dir: &Path, step: Option<u8>, cx: &mut App) {
    update(dir, false, cx, |e| e.step = step);
}

/// Fold the card to one line, or open it again. Opening a cancelled one picks it back up:
/// whatever the service finished meanwhile is fetched.
pub fn fold(dir: &Path, folded: bool, cx: &mut App) {
    let resume = !folded && cx.try_global::<Store>().and_then(|s| s.entries.get(dir)).is_some_and(|e| e.record.cancelled);
    update(dir, true, cx, |e| {
        e.record.collapsed = Some(folded);
        if resume {
            e.record.cancelled = false;
        }
    });
    if resume {
        poll(dir, cx);
    }
}

/// Stop waiting on it: no more asking the service, and the card folds. The service has no way
/// to stop a job yet, so what was started there still finishes (the views' image is already
/// spent; a model costs nothing unless imported); opening the card again fetches it.
pub fn cancel(dir: &Path, cx: &mut App) {
    update(dir, true, cx, |e| {
        e.record.cancelled = true;
        e.record.collapsed = Some(true);
        e.building = None;
        e.working = None;
    });
}

/// Show set `set` of the views (a version), or the latest with `None`.
pub fn show_set(dir: &Path, set: Option<String>, cx: &mut App) {
    update(dir, false, cx, |e| e.shown_set = set);
}

/// Pick the engine for the next model.
pub fn pick_engine(dir: &Path, engine: &str, cx: &mut App) {
    let engine = engine.to_string();
    update(dir, false, cx, |e| e.engine = Some(engine));
}

pub fn pick_detail(dir: &Path, detail: &str, cx: &mut App) {
    let detail = detail.to_string();
    update(dir, false, cx, |e| e.detail = Some(detail));
}

pub fn pick_finish(dir: &Path, finish: &str, cx: &mut App) {
    let finish = finish.to_string();
    update(dir, false, cx, |e| e.finish = Some(finish));
}

/// What the next model is asked for: the engine (picked, else the kind's default), the detail
/// (picked, else the catalog's for a faceted or a smooth model) and the finish, only when the
/// engine has finishes (picked, else faceted for a low-poly asset and smooth otherwise).
pub fn model_choice(e: &Entry, catalog: Option<&Catalog>) -> (String, Option<String>, Option<String>) {
    let asset = e.asset.as_ref();
    let kind = asset.and_then(|a| catalog?.kind(&a.kind));
    let engine = e.engine.clone().or_else(|| kind.map(|k| k.default_engine.clone())).unwrap_or_default();
    let low_poly = flow::low_poly(asset.and_then(|a| a.fields.get("style")).and_then(Value::as_str));
    let finish = catalog.and_then(|c| c.engine(&engine)).filter(|en| !en.finishes.is_empty()).and_then(|en| match &e.finish {
        Some(f) if en.finishes.contains(f) => Some(f.clone()),
        _ => flow::default_finish(en, low_poly),
    });
    let faceted = low_poly || finish.as_deref() == Some("faceted");
    let detail = match (&e.detail, catalog) {
        (Some(d), Some(c)) if c.detail(d).is_some() => Some(d.clone()),
        (_, Some(c)) => c.detail_for(faceted).map(|d| d.id.clone()),
        (picked, None) => picked.clone(),
    };
    (engine, detail, finish)
}

/// A new asset in `project`: its folder (another name's if taken) and record, then the
/// service makes its first set of views (one image spent).
#[allow(clippy::too_many_arguments)]
pub fn create(
    project: &Path,
    kind: &str,
    name: &str,
    description: &str,
    fields: BTreeMap<String, Value>,
    references: Vec<PathBuf>,
    origin: Option<wundur_core::assets::Origin>,
    cx: &mut App,
) -> Result<PathBuf, String> {
    let (typed, description) = (name.trim(), description.trim());
    if description.is_empty() {
        return Err("Say what it is: describe your asset.".into());
    }
    // Without a name the service names it from the description; the folder takes the name
    // shown greyed in the box, which is made the same way.
    let suggested = flow::suggested_name(description);
    let name = if typed.is_empty() { suggested.as_str() } else { typed };
    if references.len() > flow::REFERENCES {
        return Err(format!("At most {} reference pictures.", flow::REFERENCES));
    }
    // Each picture checked before anything is made (its bytes are read off the UI thread).
    for r in &references {
        let ext = r.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let shown = r.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp") {
            return Err(format!("{shown}: references are PNG, JPEG or WebP pictures."));
        }
        match std::fs::metadata(r) {
            Ok(m) if m.len() > flow::REFERENCE_BYTES => return Err(format!("{shown} is over 10 MB; use a smaller picture.")),
            Ok(_) => {}
            Err(e) => return Err(format!("{shown}: {e}")),
        }
    }
    let mut dir = wundur_core::assets::folder_for(project, kind, name);
    let mut n = 2;
    while dir.join(flow::RECORD).exists() || dir.join(wundur_core::assets::RECORD).exists() {
        dir = wundur_core::assets::folder_for(project, kind, &format!("{name} {n}"));
        n += 1;
    }
    // The pictures kept with the asset, in its folder (the card shows them from there).
    let mut kept = Vec::new();
    for (n, r) in references.iter().enumerate() {
        let ext = r.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "png".into());
        let file = format!("references/reference-{}.{ext}", n + 1);
        std::fs::create_dir_all(dir.join("references")).and_then(|_| std::fs::copy(r, dir.join(&file))).map_err(|e| format!("Couldn't keep a reference picture: {e}"))?;
        kept.push(file);
    }
    let record = Record { kind: kind.into(), name: name.into(), description: description.into(), fields: fields.clone(), origin, references: kept.clone(), ..Default::default() };
    flow::save(&dir, &record).map_err(|e| format!("Couldn't save the asset: {e}"))?;
    let store = cx.default_global::<Store>();
    store.entries.insert(dir.clone(), Entry { record, project: project.to_path_buf(), working: Some("Starting"), polling: true, ..Default::default() });
    store.revision += 1;
    let (kind, typed, description, folder) = (kind.to_string(), typed.to_string(), description.to_string(), dir.clone());
    let pictures: Vec<PathBuf> = kept.iter().map(|f| dir.join(f)).collect();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx
            .background_spawn(async move {
                let encoded = pictures.iter().map(|p| flow::reference(p)).collect::<Result<Vec<_>, _>>().map_err(|e| (e, false))?;
                flow::create(&kind, (!typed.is_empty()).then_some(typed.as_str()), &description, &fields, &encoded).map_err(|e| flow::explain(&e))
            })
            .await;
        cx.update(|cx| {
            update(&folder, true, cx, |e| {
                e.working = None;
                e.polling = false;
                match got {
                    Ok(asset) => {
                        e.record.id = asset.id.clone();
                        // The name the service gave it, when none was typed.
                        if !asset.name.trim().is_empty() {
                            e.record.name = asset.name.clone();
                        }
                        e.asset = Some(asset);
                        e.error = None;
                    }
                    Err(err) => e.error = Some(err),
                }
            });
            refresh_allowance(cx);
            poll(&folder, cx);
        });
    })
    .detach();
    cx.refresh_windows();
    Ok(dir)
}

/// Ask the service for something about the asset in `dir` that answers with the asset (a new
/// set, an approval), shown as `working` meanwhile; then keep up with it.
fn ask(dir: &Path, working: &'static str, cx: &mut App, call: impl FnOnce(&str) -> Result<Asset, Error> + Send + 'static) {
    let Some(id) = entry(dir, cx).map(|e| e.record.id).filter(|id| !id.is_empty()) else { return };
    update(dir, false, cx, |e| {
        e.working = Some(working);
        e.error = None;
    });
    let folder = dir.to_path_buf();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx.background_spawn(async move { call(&id) }).await;
        cx.update(|cx| {
            update(&folder, false, cx, |e| {
                e.working = None;
                match got {
                    Ok(asset) => e.asset = Some(asset),
                    Err(err) => e.error = Some(flow::explain(&err)),
                }
            });
            poll(&folder, cx);
        });
    })
    .detach();
}

/// The latest set changed by `instruction` (the whole set; one image).
pub fn change(dir: &Path, instruction: &str, cx: &mut App) {
    let Some(from) = latest_set(dir, cx) else { return };
    let instruction = instruction.trim().to_string();
    if instruction.is_empty() {
        return;
    }
    show_set(dir, None, cx);
    ask(dir, "Changing the views", cx, move |id| flow::new_set(id, &from, Some(&instruction)));
}

/// A fresh set from the description (one image).
pub fn retry(dir: &Path, cx: &mut App) {
    let Some(from) = latest_set(dir, cx) else { return };
    show_set(dir, None, cx);
    ask(dir, "Starting again", cx, move |id| flow::new_set(id, &from, None));
}

/// Approve set `set`: the one models are made from.
pub fn approve(dir: &Path, set: &str, cx: &mut App) {
    let set = set.to_string();
    show_step(dir, None, cx);
    ask(dir, "Approving", cx, move |id| flow::approve(id, &set));
}

/// The set a change or a fresh start comes from: the version shown, else the latest.
fn latest_set(dir: &Path, cx: &mut App) -> Option<String> {
    let e = entry(dir, cx)?;
    let asset = e.asset?;
    e.shown_set.filter(|s| asset.sets.iter().any(|x| &x.id == s)).or_else(|| asset.latest_set().map(|s| s.id.clone()))
}

/// Start a model on the picked engine (or the kind's default) from the approved set; or, when
/// `reroll_of` is given, another attempt after it. Nothing the person sees is spent.
pub fn model(dir: &Path, reroll_of: Option<String>, cx: &mut App) {
    let Some(e) = entry(dir, cx) else { return };
    let Some(asset) = e.asset.clone() else { return };
    let (catalog, _) = catalog(cx);
    let (engine, detail, finish) = model_choice(&e, catalog.as_ref());
    if asset.approved_set().is_none() {
        return;
    }
    let before = asset.models.len();
    update(dir, false, cx, |e| {
        e.working = Some(if reroll_of.is_some() { "Rebuilding" } else { "Starting the model" });
        e.error = None;
        e.step = None;
        e.building = Some((before, std::time::Instant::now()));
    });
    let (id, folder) = (asset.id.clone(), dir.to_path_buf());
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx
            .background_spawn(async move {
                flow::start_model(&id, (!engine.is_empty()).then_some(engine.as_str()), detail.as_deref(), finish.as_deref()).and_then(|_| flow::get(&id))
            })
            .await;
        cx.update(|cx| {
            update(&folder, false, cx, |e| {
                e.working = None;
                match got {
                    Ok(asset) => e.asset = Some(asset),
                    // One is already being made (an earlier click the card lost track of):
                    // that's the one to wait for, not an error.
                    Err(Error::Api(a)) if a.code == "job_in_progress" => {}
                    Err(err) => {
                        e.building = None;
                        e.error = Some(flow::explain(&err));
                    }
                }
            });
            poll(&folder, cx);
        });
    })
    .detach();
}

/// Import model attempt `model`: spends its engine's credits, and saves the files it brings
/// into the asset's folder (the only spend of model credits; once per attempt).
pub fn import(dir: &Path, model: &str, cx: &mut App) {
    let Some(e) = entry(dir, cx) else { return };
    let name = flow::file_name(&e.record.name, "glb").trim_end_matches(".glb").to_string();
    let asset_id = e.record.id.clone();
    update(dir, false, cx, |e| {
        e.working = Some("Importing");
        e.error = None;
    });
    let (model, folder) = (model.to_string(), dir.to_path_buf());
    let into = dir.to_path_buf();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx
            .background_spawn(async move {
                let delivered = flow::import(&asset_id, &model).map_err(|e| flow::explain(&e))?;
                let mut saved = Vec::new();
                // The model file first (links expire: fetched right away), then anything else.
                let Some(url) = delivered.model_url().map(str::to_string) else {
                    return Err(("Wundur's server sent no model file. Import again to fetch it (it isn't charged twice).".to_string(), false));
                };
                let file = format!("{name}.glb");
                flow::download(&url, &into.join(&file)).map_err(|e| (format!("The model was imported, but saving {file} failed: {e}. Import again to fetch it (it isn't charged twice)."), false))?;
                saved.push(file);
                for (n, f) in delivered.files.iter().filter(|f| f.url != url).enumerate() {
                    let base = format!("{name}-{}{}", if f.kind.is_empty() { "file" } else { &f.kind }, if n == 0 { String::new() } else { format!("-{}", n + 1) });
                    let file = format!("{base}.{}", if f.format.is_empty() { "bin" } else { &f.format });
                    flow::download(&f.url, &into.join(&file)).map_err(|e| (format!("The model was imported, but saving {file} failed: {e}. Import again to fetch it (it isn't charged twice)."), false))?;
                    saved.push(file);
                }
                Ok::<_, (String, bool)>((model, saved))
            })
            .await;
        cx.update(|cx| {
            update(&folder, true, cx, |e| {
                e.working = None;
                match got {
                    Ok((model, saved)) => {
                        e.just_done = true;
                        e.record.imported = Some(model);
                        e.record.files = saved;
                    }
                    Err(err) => e.error = Some(err),
                }
            });
            poll(&folder, cx);
        });
    })
    .detach();
}

/// Not wanted (optional; frees the preview on the service).
pub fn reject(dir: &Path, model: &str, cx: &mut App) {
    let Some(id) = entry(dir, cx).map(|e| e.record.id) else { return };
    let model = model.to_string();
    let folder = dir.to_path_buf();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let _ = cx.background_spawn(async move { flow::reject(&id, &model) }).await;
        cx.update(|cx| poll(&folder, cx));
    })
    .detach();
}

/// Show model attempt `model`'s preview in the 3D viewer: its reduced mesh downloaded into
/// Wundur's cache (not the project: it isn't the person's until imported), then `then` with
/// the model to show in the model viewer.
pub fn preview(dir: &Path, model: &str, cx: &mut App, then: impl FnOnce(PathBuf, &mut App) + 'static) {
    let Some(e) = entry(dir, cx) else { return };
    let Some(m) = e.asset.as_ref().and_then(|a| a.models.iter().find(|m| m.id == model)).cloned() else { return };
    let Some(url) = m.preview_link().map(str::to_string) else {
        update(dir, false, cx, |e| e.error = Some(("This preview has no 3D mesh to show yet.".into(), false)));
        return;
    };
    let Some(cache) = dirs::cache_dir().map(|d| d.join("wundur-studio").join("previews").join(&e.record.id)) else { return };
    let (folder, model_id) = (dir.to_path_buf(), model.to_string());
    let known = e.previews.get(model).cloned();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let file = cache.join(format!("{model_id}.glb"));
        let got = match known {
            Some(p) if p.is_file() => Ok(p),
            _ => {
                let f = file.clone();
                cx.background_spawn(async move { flow::download(&url, &f).map(|_| f) }).await
            }
        };
        cx.update(|cx| match got {
            Ok(path) => {
                update(&folder, false, cx, |e| {
                    e.previews.insert(model_id, path.clone());
                });
                then(path, cx);
            }
            Err(err) => update(&folder, false, cx, |e| e.error = Some((format!("Couldn't fetch the preview: {err}"), false))),
        });
    })
    .detach();
}

/// Keep up with the asset in `dir` while anything is being made: fetch it every few seconds,
/// save each finished set of views into its folder, stop when nothing is left to wait for.
fn poll(dir: &Path, cx: &mut App) {
    let Some(e) = cx.try_global::<Store>().and_then(|s| s.entries.get(dir)).cloned() else { return };
    if e.polling || e.record.id.is_empty() || e.record.cancelled {
        return;
    }
    update(dir, false, cx, |e| e.polling = true);
    let folder = dir.to_path_buf();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut first = true;
        loop {
            if !first {
                cx.background_executor().timer(POLL).await;
            }
            first = false;
            // Cancelled: the person stopped waiting on it.
            let Some(record) = cx.update(|cx| cx.try_global::<Store>().and_then(|s| s.entries.get(&folder)).map(|e| e.record.clone()).filter(|r| !r.cancelled)) else { break };
            let into = folder.clone();
            let got = cx
                .background_spawn(async move {
                    let asset = flow::get(&record.id)?;
                    // Reference pictures this folder doesn't have yet (an asset started elsewhere).
                    let mut references = Vec::new();
                    if record.references.is_empty() {
                        for (n, r) in asset.references.iter().enumerate() {
                            let file = format!("references/reference-{}.png", n + 1);
                            if flow::download(&r.url, &into.join(&file)).is_ok() {
                                references.push(file);
                            }
                        }
                    }
                    // Each finished set's views, saved once into the folder.
                    let mut saved: Vec<(String, Vec<String>)> = Vec::new();
                    for set in asset.sets.iter().filter(|s| s.is_ready() && !record.views.contains_key(&s.id)) {
                        let mut files = Vec::new();
                        for v in &set.views {
                            let file = format!("views/{}/{}", set.id, flow::file_name(&v.label, "png"));
                            if flow::download(&v.url, &into.join(&file)).is_ok() {
                                files.push(file);
                            }
                        }
                        if files.len() == set.views.len() {
                            saved.push((set.id.clone(), files));
                        }
                    }
                    Ok::<_, Error>((asset, saved, references))
                })
                .await;
            let busy = cx
                .update(|cx| {
                    let mut busy = false;
                    update(&folder, true, cx, |e| match got {
                        Ok((asset, saved, references)) => {
                            if !references.is_empty() {
                                e.record.references = references;
                            }
                            for (set, files) in saved {
                                e.record.views.insert(set, files);
                            }
                            // A model asked for shows up (or ten minutes pass): no longer waiting on it.
                            if let Some((before, since)) = e.building
                                && (asset.models.len() > before || asset.models.iter().any(flow::Model::is_running) || since.elapsed() > Duration::from_secs(600))
                            {
                                e.building = None;
                            }
                            busy = asset.busy() || e.building.is_some() || asset.sets.iter().any(|s| s.is_ready() && !e.record.views.contains_key(&s.id));
                            e.asset = Some(asset);
                        }
                        Err(err) => e.error = Some(flow::explain(&err)),
                    });
                    busy
                });
            if !busy {
                break;
            }
        }
        cx.update(|cx| update(&folder, false, cx, |e| e.polling = false));
    })
    .detach();
}

/// The hosted assets started in the chat with session id `id`, in `project` (to put their
/// cards back when it's read in again), with the turn each was started in.
pub fn started_in(project: &Path, id: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut found = Vec::new();
    let Ok(kinds) = std::fs::read_dir(project.join("assets")) else { return found };
    for kind in kinds.flatten() {
        let Ok(names) = std::fs::read_dir(kind.path()) else { continue };
        for dir in names.flatten().map(|d| d.path()) {
            if let Some(r) = flow::load(&dir)
                && let Some(o) = &r.origin
                && o.session == id
            {
                found.push((dir, o.turn.clone()));
            }
        }
    }
    found.sort();
    found
}
