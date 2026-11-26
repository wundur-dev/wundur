//! `ask_to_choose`: the agent shows the person files it made (pictures, 3D models) and asks
//! which one, or whether one will do. The agent says what to show and how, from a fixed set of
//! ways ([`Show`]); the window lays them out and draws the buttons, which are fixed for each
//! way, so every answer has the same shape. Nothing here spends or calls anything.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// How the files are shown, and so which buttons the person gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    /// One file, large: Confirm or Reject.
    One,
    /// Two or three side by side at the same size: pick one.
    Compare,
    /// Many: pick one or more.
    Grid,
    /// An original (first) and its edit (second): keep one.
    BeforeAfter,
}

impl Show {
    pub fn parse(s: &str) -> Option<Show> {
        match s.trim().to_lowercase().replace(['-', ' '], "_").as_str() {
            "one" => Some(Show::One),
            "compare" => Some(Show::Compare),
            "grid" => Some(Show::Grid),
            "before_after" => Some(Show::BeforeAfter),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Show::One => "one",
            Show::Compare => "compare",
            Show::Grid => "grid",
            Show::BeforeAfter => "before_after",
        }
    }

    /// How many files it takes, at least and at most.
    pub fn takes(self) -> (usize, usize) {
        match self {
            Show::One => (1, 1),
            Show::Compare => (2, 3),
            Show::Grid => (2, MAX_FILES),
            Show::BeforeAfter => (2, 2),
        }
    }

    /// The way that suits this many files, when the agent didn't say.
    fn for_count(n: usize) -> Show {
        match n {
            1 => Show::One,
            2 | 3 => Show::Compare,
            _ => Show::Grid,
        }
    }
}

/// The most files one question shows.
pub const MAX_FILES: usize = 12;

/// What the agent asked: files as it named them and where they are.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub question: String,
    /// As the agent named them (what the answer names them by)...
    pub names: Vec<String>,
    /// ...and where they are.
    pub files: Vec<PathBuf>,
    pub show: Show,
    /// Where the person is with it.
    pub state: State,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Waiting,
    /// Answered: which files they picked (by place in `files`; none for Reject), and their note.
    Answered { picked: Vec<usize>, note: String },
    /// The question went away unanswered (its session closed).
    Withdrawn,
}

/// Whether the window can show this file: a picture or a 3D model.
pub fn is_picture(path: &Path) -> bool {
    matches!(path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("png" | "jpg" | "jpeg" | "webp" | "gif"))
}

pub fn is_model(path: &Path) -> bool {
    matches!(path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("glb" | "gltf"))
}

/// A path as an agent gives it: in the project, absolute, or `~/…`.
fn resolve(cwd: &Path, given: &str) -> PathBuf {
    match (given.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if Path::new(given).is_absolute() => PathBuf::from(given),
        _ => cwd.join(given),
    }
}

/// The call's arguments as a question, or why they can't be one. `wait` is read apart
/// ([`waits`]).
pub fn parse(cwd: &Path, args: &Value) -> Result<Choice, String> {
    let question = args.get("question").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    if question.is_empty() {
        return Err("ask_to_choose needs a question: what the person is choosing, e.g. \"Which coin fits the HUD?\".".into());
    }
    let names: Vec<String> = match args.get("files") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect(),
        Some(Value::String(s)) => s.split([',', '\n']).map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect(),
        _ => Vec::new(),
    };
    if names.is_empty() {
        return Err("ask_to_choose needs files: the pictures or models to show, paths in the project.".into());
    }
    let show = match args.get("show").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
        Some(s) => Show::parse(s).ok_or_else(|| format!("show is one of one, compare, grid, before_after (not \"{s}\")."))?,
        None => Show::for_count(names.len()),
    };
    let (least, most) = show.takes();
    if names.len() < least || names.len() > most {
        let wants = if least == most { format!("{least}") } else { format!("{least} to {most}") };
        return Err(format!("show: {} takes {wants} files; {} were given.", show.name(), names.len()));
    }
    let mut files = Vec::new();
    for name in &names {
        let path = resolve(cwd, name);
        if !is_picture(&path) && !is_model(&path) {
            return Err(format!("{name} isn't a picture (PNG, JPEG, WebP, GIF) or a 3D model (GLB, glTF)."));
        }
        if !path.is_file() {
            return Err(format!("{name}: no such file."));
        }
        files.push(path);
    }
    Ok(Choice { question, names, files, show, state: State::Waiting })
}

/// Whether the call waits for the answer (the default) or returns at once.
pub fn waits(args: &Value) -> bool {
    args.get("wait").and_then(Value::as_bool).unwrap_or(true)
}

/// The labels of a way's buttons: Confirm/Reject for one, Keep the edit/Keep the original for
/// before and after, Pick for the rest.
pub fn buttons(show: Show) -> &'static [&'static str] {
    match show {
        Show::One => &["Confirm", "Reject"],
        Show::Compare => &["Pick"],
        Show::Grid => &["Pick these"],
        Show::BeforeAfter => &["Keep the edit", "Keep the original"],
    }
}

/// What a button picks, with the files selected (by place) where that matters.
pub fn picks(show: Show, button: &str, selected: &[usize]) -> Option<Vec<usize>> {
    match (show, button) {
        (Show::One, "Confirm") => Some(vec![0]),
        (Show::One, "Reject") => Some(Vec::new()),
        (Show::BeforeAfter, "Keep the edit") => Some(vec![1]),
        (Show::BeforeAfter, "Keep the original") => Some(vec![0]),
        (Show::Compare, "Pick") if selected.len() == 1 => Some(selected.to_vec()),
        (Show::Grid, "Pick these") if !selected.is_empty() => {
            let mut s = selected.to_vec();
            s.sort_unstable();
            s.dedup();
            Some(s)
        }
        _ => None,
    }
}

/// The answer as the agent gets it: a line it can read and the same as data.
pub fn answer_text(choice: &Choice, picked: &[usize], note: &str) -> String {
    let names: Vec<&str> = picked.iter().filter_map(|&i| choice.names.get(i).map(String::as_str)).collect();
    let decision = match (choice.show, names.is_empty()) {
        (Show::One, true) => "rejected it".to_string(),
        (Show::One, false) => "confirmed it".to_string(),
        (_, _) => format!("picked {}", names.join(", ")),
    };
    let note_line = if note.trim().is_empty() { String::new() } else { format!(" Their note: \"{}\".", note.trim()) };
    let data = json!({ "picked": names, "note": note.trim() });
    format!("The person answered \"{}\": they {decision}.{note_line}\n{data}", choice.question)
}

/// What the agent is told when it didn't wait, or stopped waiting.
pub fn not_yet(late: bool) -> String {
    if late {
        "{\"picked\": null, \"note\": \"\", \"none_yet\": true}\nThe person hasn't answered yet; the question stays open in the chat. Their answer comes to this session as a new message when they give it: carry on with other work or finish your turn, and don't ask again.".into()
    } else {
        "Shown to the person in the chat. Their answer comes to this session as a new message: carry on with other work or finish your turn, and don't ask again.".into()
    }
}

/// The answer framed as a message for the agent, when it comes after the call returned.
pub fn late_message(text: &str) -> String {
    format!("[Wundur: the answer to your ask_to_choose question.]\n\n{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(dir: &Path, names: &[&str]) {
        for n in names {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
    }

    #[test]
    fn a_question_needs_files_that_are_there_and_shown_the_right_way() {
        let dir = std::env::temp_dir().join(format!("wb-choices-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        files(&dir, &["a.png", "b.png", "c.png", "d.glb", "notes.txt"]);
        let cwd = dir.as_path();
        let ok = parse(cwd, &json!({ "question": "Which?", "files": ["a.png", "b.png"] })).unwrap();
        assert_eq!(ok.show, Show::Compare, "two files and no way given: compare");
        assert_eq!(parse(cwd, &json!({ "question": "Ok?", "files": "d.glb" })).unwrap().show, Show::One, "a model, alone");
        let err = |args: Value| parse(cwd, &args).unwrap_err();
        assert!(err(json!({ "files": ["a.png"] })).contains("question"));
        assert!(err(json!({ "question": "Which?", "files": ["a.png", "missing.png"] })).contains("no such file"));
        assert!(err(json!({ "question": "Which?", "files": ["notes.txt"] })).contains("isn't a picture"));
        assert!(err(json!({ "question": "Which?", "files": ["a.png"], "show": "compare" })).contains("2 to 3"));
        assert!(err(json!({ "question": "Which?", "files": ["a.png", "b.png", "c.png"], "show": "before_after" })).contains("takes 2"));
        assert!(err(json!({ "question": "Which?", "files": ["a.png"], "show": "carousel" })).contains("one of"));
        assert!(waits(&json!({})) && !waits(&json!({ "wait": false })), "waits unless told not to");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_way_has_its_buttons_and_answers_one_shape() {
        assert_eq!(picks(Show::One, "Confirm", &[]), Some(vec![0]));
        assert_eq!(picks(Show::One, "Reject", &[]), Some(vec![]));
        assert_eq!(picks(Show::BeforeAfter, "Keep the edit", &[]), Some(vec![1]), "the edit is the second file");
        assert_eq!(picks(Show::Compare, "Pick", &[]), None, "nothing selected: nothing to pick");
        assert_eq!(picks(Show::Compare, "Pick", &[0, 2]), None, "compare picks one");
        assert_eq!(picks(Show::Grid, "Pick these", &[3, 1, 3]), Some(vec![1, 3]));
        assert_eq!(picks(Show::Grid, "Confirm", &[1]), None, "a button the way doesn't have");
        let choice = Choice {
            question: "Which coin?".into(),
            names: vec!["coin-1.png".into(), "coin-2.png".into()],
            files: vec![],
            show: Show::Compare,
            state: State::Waiting,
        };
        let text = answer_text(&choice, &[1], " brighter ");
        assert!(text.contains("picked coin-2.png") && text.contains("\"note\":\"brighter\""), "{text}");
        let one = Choice { show: Show::One, ..choice };
        assert!(answer_text(&one, &[], "").contains("rejected it"));
    }
}
