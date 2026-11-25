//! "Help me choose": which engines fit a game, best first, and why.
//!
//! The New Project wizard asks where the game runs, 2D or 3D (or isometric, as drawn art or 3D
//! models), where the camera is, and code or an editor. [`recommend`] turns the answers into a
//! ranked list of the engines that can make it, each with a short reason. What each engine is
//! good at comes from its pack's `fit` section ([`Fit`]), not from code that names engines, so an
//! installed engine pack takes part like the built-in ones.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::engines::{Dimension, Engine};

/// How well an engine does one thing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Best,
    #[default]
    Works,
    /// It can't: the engine is left out of the list.
    No,
}

impl Level {
    fn score(self) -> i32 {
        match self {
            Level::Best => 2,
            Level::Works => 1,
            Level::No => 0,
        }
    }
}

/// One fact about an engine: how well, and a few words on it for the reason line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rating {
    pub level: Level,
    pub note: String,
}

/// The `fit` section of an engine pack. Anything it doesn't rate counts as "works".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fit {
    /// By [`Platform::id`].
    pub platforms: BTreeMap<String, Rating>,
    /// By [`Workflow::id`].
    pub workflow: BTreeMap<String, Rating>,
    /// By camera id (see [`cameras`]).
    pub cameras: BTreeMap<String, Rating>,
    /// How well agents work in it, higher is better; breaks ties.
    pub agent_fit: i32,
}

/// Where the game runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    Web,
    Desktop,
    Mobile,
    Consoles,
}

impl Platform {
    pub const ALL: [Platform; 4] = [Platform::Web, Platform::Desktop, Platform::Mobile, Platform::Consoles];

    pub fn id(self) -> &'static str {
        match self {
            Platform::Web => "web",
            Platform::Desktop => "desktop",
            Platform::Mobile => "mobile",
            Platform::Consoles => "consoles",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::Web => "Web browser",
            Platform::Desktop => "Desktop",
            Platform::Mobile => "Mobile",
            Platform::Consoles => "Consoles",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }
}

/// Code only, or a visual editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Workflow {
    Code,
    Editor,
}

impl Workflow {
    pub fn id(self) -> &'static str {
        match self {
            Workflow::Code => "code",
            Workflow::Editor => "editor",
        }
    }
}

/// A camera choice: its id, title and a few words.
pub struct Camera {
    pub id: &'static str,
    pub title: &'static str,
    pub text: &'static str,
}

/// The camera id an isometric game gets (the question is skipped).
pub const ISOMETRIC: &str = "isometric";

/// The cameras offered for a 2D or 3D game.
pub fn cameras(dimension: Dimension) -> &'static [Camera] {
    match dimension {
        Dimension::TwoD => &[
            Camera { id: "side", title: "Side view", text: "Platformers and side-scrollers." },
            Camera { id: "top-down", title: "Top-down", text: "Adventure, shooters, racing from above." },
            Camera { id: "screen", title: "One fixed screen", text: "Puzzle, card and board games." },
        ],
        Dimension::ThreeD => &[
            Camera { id: "first-person", title: "First person", text: "Through the player's eyes." },
            Camera { id: "third-person", title: "Behind the player", text: "Third person action and adventure." },
            Camera { id: "overhead", title: "Follows from above", text: "Top-down and strategy." },
            Camera { id: "side-on", title: "Side-on", text: "A 3D side-scroller." },
            Camera { id: "chase", title: "Chase", text: "Racing and flying." },
        ],
    }
}

/// What the person answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answers {
    /// Where it runs (at least one).
    pub platforms: Vec<Platform>,
    /// The one that comes first, if they said; it counts double.
    pub first: Option<Platform>,
    pub dimension: Dimension,
    /// A camera id from [`cameras`], or [`ISOMETRIC`].
    pub camera: Option<String>,
    /// None: they don't mind.
    pub workflow: Option<Workflow>,
}

/// One engine in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    /// Its place in the engines given to [`recommend`].
    pub index: usize,
    pub score: i32,
    /// Why, in a sentence.
    pub reason: String,
}

/// The engines that can make the game, best first. `ready[i]` says whether engine `i` has what it
/// needs on this machine (breaks ties; a missing slot counts as ready).
pub fn recommend(engines: &[Engine], answers: &Answers, ready: &[bool]) -> Vec<Pick> {
    // (pick, how it does on the platform that comes first, ready, agent fit)
    let mut picks: Vec<(Pick, i32, bool, i32)> = Vec::new();
    'engines: for (index, engine) in engines.iter().enumerate() {
        let spec = &engine.spec;
        if !spec.dimensions.contains(&answers.dimension) {
            continue;
        }
        let fit = &spec.fit;
        // (weight, rating) for each answered fact, strongest-weighted first.
        let mut facts: Vec<(i32, Rating)> = Vec::new();
        let mut platforms = answers.platforms.clone();
        if let Some(first) = answers.first.filter(|f| platforms.contains(f)) {
            platforms.retain(|p| *p != first);
            platforms.insert(0, first);
        }
        for p in &platforms {
            let weight = if answers.first == Some(*p) { 2 } else { 1 };
            facts.push((weight, fit.platforms.get(p.id()).cloned().unwrap_or_default()));
        }
        if let Some(camera) = &answers.camera {
            facts.push((1, fit.cameras.get(camera).cloned().unwrap_or_default()));
        }
        if let Some(w) = answers.workflow {
            facts.push((1, fit.workflow.get(w.id()).cloned().unwrap_or_default()));
        }
        let mut score = 0;
        for (weight, r) in &facts {
            if r.level == Level::No {
                continue 'engines;
            }
            score += weight * r.level.score();
        }
        let first = answers.first.and_then(|f| fit.platforms.get(f.id())).map_or(0, |r| r.level.score());
        let reason = reason(&facts).unwrap_or_else(|| spec.tagline.clone());
        picks.push((Pick { index, score, reason }, first, ready.get(index).copied().unwrap_or(true), fit.agent_fit));
    }
    // Ties go to the engine best on the platform that comes first, then one this machine has,
    // then the one agents work in best. Stable: still-equal engines keep the cards' order.
    picks.sort_by(|a, b| b.0.score.cmp(&a.0.score).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)).then(b.3.cmp(&a.3)));
    picks.into_iter().map(|p| p.0).collect()
}

/// The two strongest things going for it, then its biggest catch.
fn reason(facts: &[(i32, Rating)]) -> Option<String> {
    let mut parts: Vec<&str> = facts.iter().filter(|(_, r)| r.level == Level::Best && !r.note.is_empty()).map(|(_, r)| r.note.as_str()).collect();
    parts.dedup();
    parts.truncate(2);
    if let Some((_, catch)) = facts.iter().find(|(_, r)| r.level == Level::Works && !r.note.is_empty()) {
        parts.push(catch.note.as_str());
    }
    let mut parts = parts.into_iter();
    let first = parts.next()?;
    let mut out: String = first.chars().take(1).flat_map(char::to_uppercase).chain(first.chars().skip(1)).collect();
    for p in parts {
        out.push_str("; ");
        out.push_str(p);
    }
    out.push('.');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::all;

    fn names(answers: Answers) -> Vec<String> {
        let engines = all(&[]);
        recommend(&engines, &answers, &[]).into_iter().map(|p| engines[p.index].spec.name.clone()).collect()
    }

    fn ask(platforms: &[Platform], first: Option<Platform>, dimension: Dimension, camera: Option<&str>, workflow: Option<Workflow>) -> Answers {
        Answers { platforms: platforms.to_vec(), first, dimension, camera: camera.map(str::to_string), workflow }
    }

    use Dimension::{ThreeD, TwoD};
    use Platform::{Consoles, Desktop, Mobile, Web};

    #[test]
    fn the_worked_examples() {
        assert_eq!(names(ask(&[Web], None, TwoD, Some("side"), None)), ["Phaser", "Godot", "Unity"]);
        assert_eq!(names(ask(&[Mobile], None, ThreeD, Some("first-person"), None)), ["Godot", "Unity", "Three.js"]);
        assert_eq!(names(ask(&[Web, Mobile], Some(Web), TwoD, Some("side"), None)), ["Phaser", "Godot", "Unity"]);
        assert_eq!(names(ask(&[Web, Mobile], Some(Mobile), TwoD, Some("side"), None)), ["Godot", "Unity", "Phaser"]);
        assert_eq!(names(ask(&[Web, Desktop, Mobile], None, TwoD, Some(ISOMETRIC), None)), ["Godot", "Unity", "Phaser"]);
        assert_eq!(names(ask(&[Web, Desktop, Mobile], None, ThreeD, Some(ISOMETRIC), None)), ["Godot", "Unity", "Three.js"]);
        assert_eq!(names(ask(&[Web], None, ThreeD, Some("chase"), Some(Workflow::Code))), ["Three.js", "Godot", "Unity"]);
        assert_eq!(names(ask(&[Desktop], None, TwoD, Some("side"), Some(Workflow::Editor))), ["Godot", "Unity"]);
        assert_eq!(names(ask(&[Consoles], None, TwoD, None, None)), ["Unity", "Godot"]);
        assert_eq!(names(ask(&[Consoles, Web], None, ThreeD, None, None)), ["Unity", "Godot"]);
        // Even tied on points, mobile first puts the native engines ahead of a web view.
        assert_eq!(names(ask(&[Web, Mobile], Some(Mobile), ThreeD, Some(ISOMETRIC), Some(Workflow::Code))), ["Godot", "Unity", "Three.js"]);
        assert_eq!(names(ask(&[Web, Mobile], None, ThreeD, Some(ISOMETRIC), Some(Workflow::Code))), ["Three.js", "Godot", "Unity"]);
    }

    #[test]
    fn engines_that_cant_make_it_are_left_out() {
        assert!(!names(ask(&[Web], None, ThreeD, None, None)).contains(&"Phaser".to_string()));
        assert!(!names(ask(&[Web], None, TwoD, None, None)).contains(&"Three.js".to_string()));
    }

    #[test]
    fn ties_go_to_what_is_installed() {
        let engines = all(&[]);
        let unity = engines.iter().position(|e| e.spec.name == "Unity").unwrap();
        let godot = engines.iter().position(|e| e.spec.name == "Godot").unwrap();
        let mut ready = vec![true; engines.len()];
        ready[godot] = false;
        let picks = recommend(&engines, &ask(&[Mobile], None, ThreeD, None, None), &ready);
        assert_eq!(picks[0].index, unity);
    }

    #[test]
    fn reasons_say_why_and_the_catch() {
        let engines = all(&[]);
        let picks = recommend(&engines, &ask(&[Web, Mobile], Some(Web), TwoD, Some("side"), None), &[]);
        let phaser = &picks[0].reason;
        assert!(phaser.starts_with(char::is_uppercase) && phaser.ends_with('.'), "{phaser}");
        assert!(phaser.contains("60 fps"), "{phaser}");
        for p in &picks {
            assert!(!p.reason.is_empty());
        }
    }

    #[test]
    fn a_pack_without_fit_still_takes_part() {
        let mut engines = all(&[]);
        for e in &mut engines {
            e.spec.fit = Fit::default();
        }
        let picks = recommend(&engines, &ask(&[Consoles], None, TwoD, None, None), &[]);
        assert_eq!(picks.len(), 3);
        assert!(picks.iter().all(|p| p.reason == engines[p.index].spec.tagline));
    }
}
