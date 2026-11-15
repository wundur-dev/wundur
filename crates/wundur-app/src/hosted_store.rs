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
    /// The engine picked for the next model.
    pub engine: Option<String>,
    /// Previews downloaded (outside the project), by model attempt.
    pub previews: HashMap<String, PathBuf>,
    polling: bool,
}

#[derive(Default)]
struct Store {
    entries: HashMap<PathBuf, Entry>,
    catalog: Option<Catalog>,
    /// The person's plan (`free`, `monthly`…), for which engines they may use.
    plan: Option<String>,
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
            let got = cx.background_spawn(async move { (flow::catalog(), account::shared().me_with(Auth::Key).map(|m| m.plan)) }).await;
            cx.update(|cx| {
                let store = cx.default_global::<Store>();
                store.loading = false;
                match got {
                    (Ok(c), plan) => {
                        store.catalog = Some(c);
                        store.plan = plan.ok();
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

/// Pick the engine for the next model.
pub fn pick_engine(dir: &Path, engine: &str, cx: &mut App) {
    let engine = engine.to_string();
    update(dir, false, cx, |e| e.engine = Some(engine));
}

/// A new asset in `project`: its folder (another name's if taken) and record, then the
/// service makes its first set of views (one image spent).
pub fn create(
    project: &Path,
    kind: &str,
    name: &str,
    description: &str,
    fields: BTreeMap<String, Value>,
    origin: Option<wundur_core::assets::Origin>,
    cx: &mut App,
) -> Result<PathBuf, String> {
    let (name, description) = (name.trim(), description.trim());
    if name.is_empty() || description.is_empty() {
        return Err("Give it a name and say what it is.".into());
    }
    let mut dir = wundur_core::assets::folder_for(project, kind, name);
    let mut n = 2;
    while dir.join(flow::RECORD).exists() || dir.join(wundur_core::assets::RECORD).exists() {
        dir = wundur_core::assets::folder_for(project, kind, &format!("{name} {n}"));
        n += 1;
    }
    let record = Record { kind: kind.into(), name: name.into(), description: description.into(), fields: fields.clone(), origin, ..Default::default() };
    flow::save(&dir, &record).map_err(|e| format!("Couldn't save the asset: {e}"))?;
    let store = cx.default_global::<Store>();
    store.entries.insert(dir.clone(), Entry { record, project: project.to_path_buf(), working: Some("Starting"), polling: true, ..Default::default() });
    store.revision += 1;
    let (kind, name, description, folder) = (kind.to_string(), name.to_string(), description.to_string(), dir.clone());
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx.background_spawn(async move { flow::create(&kind, &name, &description, &fields) }).await;
        cx.update(|cx| {
            update(&folder, true, cx, |e| {
                e.working = None;
                e.polling = false;
                match got {
                    Ok(asset) => {
                        e.record.id = asset.id.clone();
                        e.asset = Some(asset);
                        e.error = None;
                    }
                    Err(err) => e.error = Some(flow::explain(&err)),
                }
            });
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
    ask(dir, "Changing the views", cx, move |id| flow::new_set(id, &from, Some(&instruction)));
}

/// A fresh set from the description (one image).
pub fn retry(dir: &Path, cx: &mut App) {
    let Some(from) = latest_set(dir, cx) else { return };
    ask(dir, "Starting again", cx, move |id| flow::new_set(id, &from, None));
}

/// Approve set `set`: the one models are made from.
pub fn approve(dir: &Path, set: &str, cx: &mut App) {
    let set = set.to_string();
    ask(dir, "Approving", cx, move |id| flow::approve(id, &set));
}

fn latest_set(dir: &Path, cx: &mut App) -> Option<String> {
    entry(dir, cx)?.asset?.latest_set().map(|s| s.id.clone())
}

/// Start a model on the picked engine (or the kind's default) from the approved set; or, when
/// `reroll_of` is given, another attempt after it. Nothing the person sees is spent.
pub fn model(dir: &Path, reroll_of: Option<String>, cx: &mut App) {
    let Some(e) = entry(dir, cx) else { return };
    let Some(asset) = e.asset.clone() else { return };
    let Some(set) = asset.approved_set().map(|s| s.id.clone()) else { return };
    let (catalog, _) = catalog(cx);
    let default = catalog.as_ref().and_then(|c| c.kind(&asset.kind)).map(|k| k.default_engine.clone()).unwrap_or_default();
    let engine = e.engine.clone().unwrap_or(default);
    let previous_engine = reroll_of.as_ref().and_then(|m| asset.models.iter().find(|x| &x.id == m)).map(|m| m.engine.clone());
    update(dir, false, cx, |e| {
        e.working = Some(if reroll_of.is_some() { "Trying again" } else { "Starting the model" });
        e.error = None;
    });
    let (id, folder) = (asset.id.clone(), dir.to_path_buf());
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx
            .background_spawn(async move {
                let made = match &reroll_of {
                    // A different engine than the attempt being rerolled: say so.
                    Some(m) => flow::reroll(m, (previous_engine.as_deref() != Some(engine.as_str())).then_some(engine.as_str())),
                    None => flow::start_model(&id, &set, &engine),
                };
                made.and_then(|_| flow::get(&id))
            })
            .await;
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

/// Import model attempt `model`: spends its engine's credits, and saves the files it brings
/// into the asset's folder (the only spend of model credits; once per attempt).
pub fn import(dir: &Path, model: &str, cx: &mut App) {
    let Some(e) = entry(dir, cx) else { return };
    let name = flow::file_name(&e.record.name, "glb").trim_end_matches(".glb").to_string();
    update(dir, false, cx, |e| {
        e.working = Some("Importing");
        e.error = None;
    });
    let (model, folder) = (model.to_string(), dir.to_path_buf());
    let into = dir.to_path_buf();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let got = cx
            .background_spawn(async move {
                let delivered = flow::import(&model).map_err(|e| flow::explain(&e))?;
                let mut saved = Vec::new();
                for (n, f) in delivered.files.iter().enumerate() {
                    let base = if f.kind == "model" || n == 0 { name.clone() } else { format!("{name}-{}", f.kind) };
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
    let model = model.to_string();
    let folder = dir.to_path_buf();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let _ = cx.background_spawn(async move { flow::reject(&model) }).await;
        cx.update(|cx| poll(&folder, cx));
    })
    .detach();
}

/// Show model attempt `model`'s preview in the 3D viewer: its reduced mesh downloaded into
/// Wundur's cache (not the project: it isn't the person's until imported), then `then` with
/// the viewer's page to open.
pub fn preview(dir: &Path, model: &str, cx: &mut App, then: impl FnOnce(PathBuf, &mut App) + 'static) {
    let Some(e) = entry(dir, cx) else { return };
    let Some(m) = e.asset.as_ref().and_then(|a| a.models.iter().find(|m| m.id == model)).cloned() else { return };
    let Some(url) = m.preview.as_ref().and_then(|p| p.mesh.as_ref()).map(|mesh| mesh.url.clone()) else {
        update(dir, false, cx, |e| e.error = Some(("This preview has no 3D mesh to show yet.".into(), false)));
        return;
    };
    let Some(cache) = dirs::cache_dir().map(|d| d.join("wundur-studio").join("previews").join(&e.record.id)) else { return };
    let title = format!("{} (preview)", e.record.name);
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
                crate::asset_store::viewer_page(&cache, &path, &title, cx, then);
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
    if e.polling || e.record.id.is_empty() {
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
            let Some(record) = cx.update(|cx| cx.try_global::<Store>().and_then(|s| s.entries.get(&folder)).map(|e| e.record.clone())) else { break };
            let into = folder.clone();
            let got = cx
                .background_spawn(async move {
                    let asset = flow::get(&record.id)?;
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
                    Ok::<_, Error>((asset, saved))
                })
                .await;
            let busy = cx
                .update(|cx| {
                    let mut busy = false;
                    update(&folder, true, cx, |e| match got {
                        Ok((asset, saved)) => {
                            for (set, files) in saved {
                                e.record.views.insert(set, files);
                            }
                            busy = asset.busy() || asset.sets.iter().any(|s| s.is_ready() && !e.record.views.contains_key(&s.id));
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
