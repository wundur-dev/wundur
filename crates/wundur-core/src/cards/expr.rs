//! What a card manifest says about its data: paths into it (`data.sets[ui.set].views`),
//! conditions (`card.busy || !form.prompt`) and text with holes (`"Views v{set.number}"`).
//!
//! There is no arithmetic and nothing loops: a manifest lays data out and tests it, and the
//! service that sends the data does the working out. Anything missing reads as nothing (null),
//! never as an error, so a card drawn before its data has arrived is merely empty.

use serde_json::Value;

/// One step along a path: a name or position (`.name`, `.0`), or a looked-up one (`[expr]`).
#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Key(String),
    Index(Box<Expr>),
}

/// A path: the scope or loop name it starts at, and the steps from there.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub root: String,
    pub segs: Vec<Seg>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Lit(Value),
    Path(Path),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Cmp(Op, Box<Expr>, Box<Expr>),
}

/// A piece of a text template: text as written, or a hole with its filters (`{name|capital}`).
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Text(String),
    Hole(Expr, Vec<(String, String)>),
}

/// The filters a hole may name.
pub const FILTERS: [&str; 6] = ["or", "capital", "upper", "lower", "count", "slug"];

// ----- parsing -----

struct Parser<'a> {
    src: &'a str,
    at: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.at..]
    }

    fn space(&mut self) {
        self.at += self.rest().len() - self.rest().trim_start().len();
    }

    fn eat(&mut self, token: &str) -> bool {
        self.space();
        if self.rest().starts_with(token) {
            self.at += token.len();
            true
        } else {
            false
        }
    }

    fn fail<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at {} in `{}`", self.at + 1, self.src))
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut left = self.and()?;
        while self.eat("||") {
            left = Expr::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut left = self.not()?;
        while self.eat("&&") {
            left = Expr::And(Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, String> {
        self.space();
        // `!=` is a comparison's, never a negation.
        if self.rest().starts_with('!') && !self.rest().starts_with("!=") {
            self.at += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Result<Expr, String> {
        let left = self.value()?;
        for (token, op) in [("==", Op::Eq), ("!=", Op::Ne), ("<=", Op::Le), (">=", Op::Ge), ("<", Op::Lt), (">", Op::Gt)] {
            if self.eat(token) {
                return Ok(Expr::Cmp(op, Box::new(left), Box::new(self.value()?)));
            }
        }
        Ok(left)
    }

    fn value(&mut self) -> Result<Expr, String> {
        self.space();
        let rest = self.rest();
        let Some(first) = rest.chars().next() else { return self.fail("something is missing") };
        if first == '(' {
            self.at += 1;
            let inner = self.or()?;
            return if self.eat(")") { Ok(inner) } else { self.fail("a `)` is missing") };
        }
        if first == '\'' || first == '"' {
            let Some(end) = rest[1..].find(first) else { return self.fail("a quote isn't closed") };
            self.at += end + 2;
            return Ok(Expr::Lit(Value::String(rest[1..=end].to_string())));
        }
        if first.is_ascii_digit() || (first == '-' && rest[1..].starts_with(|c: char| c.is_ascii_digit())) {
            let len = 1 + rest[1..].find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(rest.len() - 1);
            let Ok(n) = rest[..len].parse::<f64>() else { return self.fail("not a number") };
            self.at += len;
            return Ok(Expr::Lit(number(n)));
        }
        if first.is_alphabetic() || first == '_' {
            let path = self.path()?;
            if path.segs.is_empty() {
                match path.root.as_str() {
                    "true" => return Ok(Expr::Lit(Value::Bool(true))),
                    "false" => return Ok(Expr::Lit(Value::Bool(false))),
                    "null" => return Ok(Expr::Lit(Value::Null)),
                    _ => {}
                }
            }
            return Ok(Expr::Path(path));
        }
        self.fail("unexpected text")
    }

    fn name(&mut self) -> Option<String> {
        let rest = self.rest();
        let len = rest.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-')).unwrap_or(rest.len());
        // A name never ends in a dash (so `a - b` isn't one name), and isn't empty.
        let name = rest[..len].trim_end_matches('-');
        if name.is_empty() {
            return None;
        }
        self.at += name.len();
        Some(name.to_string())
    }

    fn path(&mut self) -> Result<Path, String> {
        let Some(root) = self.name() else { return self.fail("a name is missing") };
        let mut segs = Vec::new();
        loop {
            if self.rest().starts_with('.') {
                self.at += 1;
                match self.name() {
                    Some(key) => segs.push(Seg::Key(key)),
                    None => return self.fail("a name is missing after `.`"),
                }
            } else if self.rest().starts_with('[') {
                self.at += 1;
                let inner = self.or()?;
                if !self.eat("]") {
                    return self.fail("a `]` is missing");
                }
                segs.push(Seg::Index(Box::new(inner)));
            } else {
                return Ok(Path { root, segs });
            }
        }
    }
}

/// A condition or value: `ui.step == 'views' && !card.busy`.
pub fn parse(src: &str) -> Result<Expr, String> {
    let mut p = Parser { src, at: 0 };
    let e = p.or()?;
    p.space();
    if p.at < src.len() {
        return p.fail("unexpected text");
    }
    Ok(e)
}

/// A path alone, for something written to (`form.fields[field.id]`).
pub fn parse_path(src: &str) -> Result<Path, String> {
    match parse(src)? {
        Expr::Path(p) => Ok(p),
        _ => Err(format!("`{src}` isn't a path")),
    }
}

/// The parts of a hole, split at each `|` that isn't half of an `||` or inside quotes.
fn split_filters(hole: &str) -> Vec<&str> {
    let (mut parts, mut start, mut quote) = (Vec::new(), 0, None::<char>);
    let bytes: Vec<(usize, char)> = hole.char_indices().collect();
    let mut i = 0;
    while i < bytes.len() {
        let (at, c) = bytes[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '|' => {
                if bytes.get(i + 1).is_some_and(|(_, n)| *n == '|') {
                    i += 1;
                } else {
                    parts.push(&hole[start..at]);
                    start = at + 1;
                }
            }
            None => {}
        }
        i += 1;
    }
    parts.push(&hole[start..]);
    parts
}

/// Text with holes: `"Import · {model.cost}"`. `{{` and `}}` are the braces themselves.
pub fn parse_template(src: &str) -> Result<Vec<Piece>, String> {
    let (mut pieces, mut text) = (Vec::new(), String::new());
    let mut rest = src;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("{{") || rest.starts_with("}}") {
            text.push(c);
            rest = &rest[2..];
        } else if c == '{' {
            let Some(end) = rest.find('}') else { return Err(format!("a `{{` isn't closed in `{src}`")) };
            let parts = split_filters(&rest[1..end]);
            let expr = parse(parts[0])?;
            let mut filters = Vec::new();
            for f in &parts[1..] {
                let (name, arg) = f.split_once(':').unwrap_or((f, ""));
                let name = name.trim();
                if !FILTERS.contains(&name) {
                    return Err(format!("`{name}` isn't a filter (there are: {}) in `{src}`", FILTERS.join(", ")));
                }
                filters.push((name.to_string(), arg.to_string()));
            }
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(&mut text)));
            }
            pieces.push(Piece::Hole(expr, filters));
            rest = &rest[end + 1..];
        } else {
            text.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    Ok(pieces)
}

// ----- values -----

/// A number as JSON keeps it: whole ones as integers, so they read "3" and not "3.0".
pub fn number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9e15 { Value::from(n as i64) } else { serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number) }
}

/// Whether a value counts as there: not null, false, zero, or empty.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// A value as shown in text: nothing for null, a string as it is, the rest as JSON writes them.
pub fn text_of(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 9e15 => (f as i64).to_string(),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

fn capital(text: &str) -> String {
    let mut c = text.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Text as a file name: lowercase letters and digits, dashes between, sixty at most.
fn slug(text: &str) -> String {
    let dashed: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    let mut out = dashed.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if let Some((cut, _)) = out.char_indices().nth(60) {
        out.truncate(cut);
    }
    out.trim_end_matches('-').to_string()
}

/// One step into a value. A list takes a position, `first` or `last`, or the `id` of one of
/// its items (`catalog.kinds[form.kind]`); an object takes a name.
fn step(value: &Value, key: &Value) -> Value {
    match (value, key) {
        (Value::Array(items), Value::Number(n)) => n.as_u64().and_then(|n| items.get(n as usize)).cloned().unwrap_or(Value::Null),
        (Value::Array(items), Value::String(k)) => match k.parse::<usize>() {
            Ok(n) => items.get(n).cloned().unwrap_or(Value::Null),
            Err(_) => items
                .iter()
                .find(|i| i.get("id").is_some_and(|id| text_of(id) == *k))
                .or_else(|| match k.as_str() {
                    "first" => items.first(),
                    "last" => items.last(),
                    _ => None,
                })
                .cloned()
                .unwrap_or(Value::Null),
        },
        (Value::Object(map), key) => map.get(&text_of(key)).cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// The data a manifest reads: the card's scopes (`form`, `ui`, `data`, `catalog`, `account`,
/// `card`) and, inside a repeated node, the names its loops gave (latest first to win).
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    pub root: &'a Value,
    pub locals: &'a [(String, Value)],
}

impl Scope<'_> {
    pub fn get(&self, path: &Path) -> Value {
        let mut value = match self.locals.iter().rev().find(|(name, _)| *name == path.root) {
            Some((_, v)) => v.clone(),
            None => self.root.get(&path.root).cloned().unwrap_or(Value::Null),
        };
        for seg in &path.segs {
            let key = match seg {
                Seg::Key(k) => Value::String(k.clone()),
                Seg::Index(e) => self.eval(e),
            };
            value = step(&value, &key);
            if value.is_null() {
                break;
            }
        }
        value
    }

    pub fn eval(&self, e: &Expr) -> Value {
        match e {
            Expr::Lit(v) => v.clone(),
            Expr::Path(p) => self.get(p),
            Expr::Not(e) => Value::Bool(!truthy(&self.eval(e))),
            Expr::And(a, b) => {
                let left = self.eval(a);
                if truthy(&left) { self.eval(b) } else { left }
            }
            Expr::Or(a, b) => {
                let left = self.eval(a);
                if truthy(&left) { left } else { self.eval(b) }
            }
            Expr::Cmp(op, a, b) => {
                let (a, b) = (self.eval(a), self.eval(b));
                let order = match (&a, &b) {
                    (Value::Number(x), Value::Number(y)) => x.as_f64().partial_cmp(&y.as_f64()),
                    (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
                    (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
                    (Value::Null, Value::Null) => Some(std::cmp::Ordering::Equal),
                    _ => None,
                };
                use std::cmp::Ordering::*;
                Value::Bool(match op {
                    Op::Eq => order == Some(Equal),
                    Op::Ne => order != Some(Equal),
                    Op::Lt => order == Some(Less),
                    Op::Le => matches!(order, Some(Less | Equal)),
                    Op::Gt => order == Some(Greater),
                    Op::Ge => matches!(order, Some(Greater | Equal)),
                })
            }
        }
    }

    /// Whether a condition holds. One that can't be read doesn't.
    pub fn test(&self, condition: &str) -> bool {
        parse(condition).is_ok_and(|e| truthy(&self.eval(&e)))
    }

    /// A value named by an expression (`data.sets`); null when it can't be read.
    pub fn value_at(&self, expr: &str) -> Value {
        parse(expr).map_or(Value::Null, |e| self.eval(&e))
    }

    fn hole(&self, expr: &Expr, filters: &[(String, String)]) -> Value {
        let mut value = self.eval(expr);
        for (name, arg) in filters {
            value = match name.as_str() {
                "or" if !truthy(&value) => Value::String(arg.clone()),
                "capital" => Value::String(capital(&text_of(&value))),
                "upper" => Value::String(text_of(&value).to_uppercase()),
                "lower" => Value::String(text_of(&value).to_lowercase()),
                "slug" => Value::String(slug(&text_of(&value))),
                "count" => Value::from(match &value {
                    Value::Array(a) => a.len(),
                    Value::Object(o) => o.len(),
                    Value::String(s) => s.chars().count(),
                    _ => 0,
                }),
                _ => value,
            };
        }
        value
    }

    /// A template's text, its holes filled. One that can't be read is shown as written, so a
    /// mistake is seen where it is.
    pub fn text(&self, template: &str) -> String {
        if !template.contains(['{', '}']) {
            return template.to_string();
        }
        match parse_template(template) {
            Ok(pieces) => pieces
                .iter()
                .map(|p| match p {
                    Piece::Text(t) => t.clone(),
                    Piece::Hole(e, f) => text_of(&self.hole(e, f)),
                })
                .collect(),
            Err(_) => template.to_string(),
        }
    }

    /// A manifest value filled in. A string that is one hole and nothing else (`"{form.loop}"`)
    /// gives the value itself, keeping its type; any other string gives its text; lists and
    /// objects are filled through, and an object's entries that come out null are left out.
    pub fn fill(&self, value: &Value) -> Value {
        match value {
            Value::String(s) => match parse_template(s).as_deref() {
                Ok([Piece::Hole(e, f)]) => self.hole(e, f),
                _ => Value::String(self.text(s)),
            },
            Value::Array(items) => Value::Array(items.iter().map(|i| self.fill(i)).collect()),
            Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), self.fill(v))).filter(|(_, v)| !v.is_null()).collect()),
            other => other.clone(),
        }
    }

    /// A path as plain names, its looked-up steps worked out (`form.fields[field.id]` →
    /// `form`, `fields`, `height`), for writing to. A loop's name can't be written to.
    pub fn place(&self, path: &str) -> Result<Vec<String>, String> {
        let path = parse_path(path)?;
        if self.locals.iter().any(|(name, _)| *name == path.root) {
            return Err(format!("`{}` is a loop's name, not somewhere to write", path.root));
        }
        let mut place = vec![path.root.clone()];
        for seg in &path.segs {
            place.push(match seg {
                Seg::Key(k) => k.clone(),
                Seg::Index(e) => text_of(&self.eval(e)),
            });
        }
        Ok(place)
    }
}

/// Write `value` at `place` under `root`, making the objects on the way.
pub fn write(root: &mut Value, place: &[String], value: Value) {
    let Some((last, way)) = place.split_last() else { return };
    let mut at = root;
    for key in way {
        if !at.is_object() {
            *at = Value::Object(Default::default());
        }
        at = at.as_object_mut().expect("just made").entry(key.clone()).or_insert(Value::Null);
    }
    match at {
        Value::Array(items) => {
            if let Ok(n) = last.parse::<usize>()
                && n < items.len()
            {
                items[n] = value;
            }
        }
        _ => {
            if !at.is_object() {
                *at = Value::Object(Default::default());
            }
            at.as_object_mut().expect("just made").insert(last.clone(), value);
        }
    }
}

/// What is at `place` under `root`.
pub fn read<'a>(root: &'a Value, place: &[String]) -> &'a Value {
    let mut at = root;
    for key in place {
        at = match at {
            Value::Object(map) => map.get(key).unwrap_or(&Value::Null),
            Value::Array(items) => key.parse::<usize>().ok().and_then(|n| items.get(n)).unwrap_or(&Value::Null),
            _ => &Value::Null,
        };
    }
    at
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn root() -> Value {
        json!({
            "form": { "kind": "prop", "prompt": "", "title": "A Gold Coin, top-down!", "loop": true, "seconds": 2, "fields": { "height": "tall" } },
            "ui": { "step": "views", "set": "s2" },
            "data": { "sets": [{ "id": "s1", "label": "v1" }, { "id": "s2", "label": "v2", "views": ["a", "b"] }], "cost": 3 },
            "catalog": { "kinds": [{ "id": "humanoid", "label": "Humanoid" }, { "id": "prop", "label": "Prop" }] },
            "card": { "busy": false, "width": 480 },
        })
    }

    #[test]
    fn paths_reach_into_objects_lists_and_items_by_id() {
        let root = root();
        let s = Scope { root: &root, locals: &[] };
        assert_eq!(s.value_at("ui.step"), json!("views"));
        assert_eq!(s.value_at("data.sets.0.label"), json!("v1"));
        assert_eq!(s.value_at("data.sets[1].label"), json!("v2"));
        assert_eq!(s.value_at("data.sets[ui.set].label"), json!("v2"), "a list item by its id");
        assert_eq!(s.value_at("catalog.kinds[form.kind].label"), json!("Prop"));
        assert_eq!(s.value_at("form.fields['height']"), json!("tall"));
        assert_eq!(s.value_at("data.nothing.here"), Value::Null, "missing is null, not an error");
        assert_eq!(s.value_at("data.sets.last.label"), json!("v2"));
        assert_eq!(s.value_at("data.sets.first.id"), json!("s1"));
        assert_eq!(s.value_at("(data.sets[ui.missing] || data.sets.last).id"), Value::Null, "a path starts at a name");
        assert_eq!(s.value_at("data.sets[ui.missing] || data.sets.last"), json!({ "id": "s2", "label": "v2", "views": ["a", "b"] }));
    }

    #[test]
    fn conditions_compare_and_combine() {
        let root = root();
        let s = Scope { root: &root, locals: &[] };
        assert!(s.test("ui.step == 'views'"));
        assert!(s.test("ui.step != 'model' && !card.busy"));
        assert!(s.test("!form.prompt"), "an empty string isn't there");
        assert!(s.test("card.width < 520"));
        assert!(!s.test("card.width >= 520"));
        assert!(s.test("form.prompt || form.loop"));
        assert!(s.test("(card.busy || data.cost > 2) && form.loop == true"));
        assert!(!s.test("data.cost == '3'"), "a number isn't the string of it");
        assert!(!s.test("this isn't a condition"), "one that can't be read doesn't hold");
    }

    #[test]
    fn loop_names_come_before_the_card_s_scopes() {
        let root = root();
        let locals = vec![("set".to_string(), json!({ "id": "s9", "label": "v9" })), ("data".to_string(), json!("shadowed"))];
        let s = Scope { root: &root, locals: &locals };
        assert_eq!(s.text("{set.label} of {data}"), "v9 of shadowed");
    }

    #[test]
    fn templates_fill_their_holes() {
        let root = root();
        let s = Scope { root: &root, locals: &[] };
        assert_eq!(s.text("Import · {data.cost} credits"), "Import · 3 credits");
        assert_eq!(s.text("{form.kind|capital} ({form.kind|upper})"), "Prop (PROP)");
        assert_eq!(s.text("{form.prompt|or:Nothing yet}"), "Nothing yet");
        assert_eq!(s.text("{data.sets|count} versions"), "2 versions");
        assert_eq!(s.text("{form.title|slug}.png"), "a-gold-coin-top-down.png");
        assert_eq!(s.text("{form.prompt || form.kind}"), "prop", "an `||` isn't a filter");
        assert_eq!(s.text("{{literal}} {data.nothing}"), "{literal} ");
        assert_eq!(s.text("{broken"), "{broken", "one that can't be read is shown as written");
    }

    #[test]
    fn a_lone_hole_keeps_its_value_s_type() {
        let root = root();
        let s = Scope { root: &root, locals: &[] };
        let filled = s.fill(&json!({ "text": "{form.prompt}", "seconds": "{form.seconds}", "loop": "{form.loop}", "label": "{form.seconds} s", "none": "{data.nothing}", "list": ["{form.kind}", 4] }));
        assert_eq!(filled, json!({ "text": "", "seconds": 2, "loop": true, "label": "2 s", "list": ["prop", 4] }), "null entries are left out");
    }

    #[test]
    fn places_are_worked_out_and_written() {
        let mut root = root();
        let locals = vec![("field".to_string(), json!({ "id": "build" }))];
        let place = Scope { root: &root, locals: &locals }.place("form.fields[field.id]").unwrap();
        assert_eq!(place, ["form", "fields", "build"]);
        write(&mut root, &place, json!("stocky"));
        assert_eq!(read(&root, &place), &json!("stocky"));
        write(&mut root, &["ui".into(), "open".into(), "brief".into()], json!(true));
        assert_eq!(root["ui"]["open"]["brief"], json!(true), "objects on the way are made");
        assert!(Scope { root: &root, locals: &locals }.place("field.id").is_err(), "a loop's name isn't written to");
    }

    #[test]
    fn mistakes_say_where() {
        assert!(parse("ui.step ==").unwrap_err().contains("missing"));
        assert!(parse("ui.step == 'views").unwrap_err().contains("quote"));
        assert!(parse_template("{form.kind|shout}").unwrap_err().contains("isn't a filter"));
        assert!(parse_template("{form.kind").unwrap_err().contains("isn't closed"));
    }
}
