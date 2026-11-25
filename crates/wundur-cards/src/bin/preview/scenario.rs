//! A scenario: the made-up service a card is previewed against. It sits beside the manifest
//! (`sound-effect.json` → `sound-effect.scenario.json`) and says what the card starts with,
//! the states worth looking at, and how each call is answered and after how long.
//!
//! ```json
//! {
//!   "catalog": { },
//!   "account": { "plan": "free" },
//!   "states": [
//!     { "name": "Empty" },
//!     { "name": "Done", "ui": { "step": "done" }, "data": { "files": [] } }
//!   ],
//!   "replies": {
//!     "make": { "after": 2, "data": { "id": "snd_1" } },
//!     "refresh": [{ "after": 1, "merge": { "progress": 0.5 } }, { "state": "Done" }]
//!   }
//! }
//! ```
//!
//! A reply is `data` (the answer), `merge` (the card's `data` with this laid over it), `state`
//! (a named state's `data`) or `error` (a message, or `{ "message", "code" }`), each after
//! `after` seconds. A list answers successive calls in order, the last one from then on; or,
//! when its replies have an `if` (a condition on the card's data, as a manifest writes one),
//! the first whose `if` holds answers.

use serde_json::{Map, Value};
use wundur_core::cards::Failure;

#[derive(Default)]
pub struct Scenario {
    pub catalog: Value,
    pub account: Value,
    /// Named states, in the order written: what to lay over a new card's `form`, `ui`, `data`
    /// and `card`.
    pub states: Vec<(String, Value)>,
    replies: Map<String, Value>,
}

pub struct Reply {
    pub after: f64,
    pub result: Result<Value, Failure>,
}

/// `over` laid on `base`: objects merge key by key, anything else replaces.
pub fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                merge(base.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        (base, over) => *base = over.clone(),
    }
}

impl Scenario {
    pub fn parse(source: &str) -> Result<Scenario, String> {
        let value: Value = serde_json::from_str(source).map_err(|e| format!("not JSON: {e}"))?;
        let Some(map) = value.as_object() else { return Err("a scenario is an object".into()) };
        let mut states = Vec::new();
        for state in map.get("states").and_then(Value::as_array).into_iter().flatten() {
            let Some(name) = state.get("name").and_then(Value::as_str) else { return Err("each state needs a `name`".into()) };
            states.push((name.to_string(), state.clone()));
        }
        Ok(Scenario {
            catalog: map.get("catalog").cloned().unwrap_or(Value::Null),
            account: map.get("account").cloned().unwrap_or(Value::Null),
            states,
            replies: map.get("replies").and_then(Value::as_object).cloned().unwrap_or_default(),
        })
    }

    pub fn state(&self, name: &str) -> Option<&Value> {
        self.states.iter().find(|(n, _)| n == name).map(|(_, s)| s)
    }

    /// The answer to the `nth` call named `name` (counting from 0), given the card's scopes.
    pub fn reply(&self, name: &str, nth: usize, root: &Value) -> Option<Reply> {
        let data = root.get("data").unwrap_or(&Value::Null);
        let scope = wundur_core::cards::Scope { root, locals: &[] };
        let written = match self.replies.get(name)? {
            Value::Array(list) if list.iter().any(|r| r.get("if").is_some()) => list.iter().find(|r| r.get("if").and_then(Value::as_str).is_none_or(|c| scope.test(c)))?,
            Value::Array(list) => list.get(nth).or(list.last())?,
            one => one,
        };
        let after = written.get("after").and_then(Value::as_f64).unwrap_or(0.6).max(0.);
        let result = if let Some(error) = written.get("error") {
            Err(match error {
                Value::String(message) => Failure { message: message.clone(), code: String::new() },
                other => Failure { message: other.get("message").and_then(Value::as_str).unwrap_or("It failed.").to_string(), code: other.get("code").and_then(Value::as_str).unwrap_or_default().to_string() },
            })
        } else if let Some(state) = written.get("state").and_then(Value::as_str) {
            Ok(self.state(state).and_then(|s| s.get("data")).cloned().unwrap_or(Value::Null))
        } else if let Some(over) = written.get("merge") {
            let mut merged = data.clone();
            merge(&mut merged, over);
            Ok(merged)
        } else {
            Ok(written.get("data").cloned().unwrap_or(Value::Null))
        };
        Some(Reply { after, result })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn replies_answer_in_order_and_the_last_repeats() {
        let s = Scenario::parse(
            &json!({
                "states": [{ "name": "Done", "data": { "status": "done" } }],
                "replies": {
                    "make": { "after": 2, "data": { "id": "a" } },
                    "refresh": [{ "merge": { "progress": 0.5 } }, { "state": "Done" }],
                    "import": { "error": { "message": "Out of credits.", "code": "quota" } },
                },
            })
            .to_string(),
        )
        .unwrap();
        let make = s.reply("make", 3, &Value::Null).unwrap();
        assert_eq!((make.after, make.result), (2., Ok(json!({ "id": "a" }))));
        assert_eq!(s.reply("refresh", 0, &json!({ "data": { "id": "a", "progress": 0.1 } })).unwrap().result, Ok(json!({ "id": "a", "progress": 0.5 })));
        assert_eq!(s.reply("refresh", 1, &Value::Null).unwrap().result, Ok(json!({ "status": "done" })));
        assert_eq!(s.reply("refresh", 9, &Value::Null).unwrap().result, Ok(json!({ "status": "done" })), "the last answers from then on");
        assert_eq!(s.reply("import", 0, &Value::Null).unwrap().result, Err(Failure { message: "Out of credits.".into(), code: "quota".into() }));
        assert!(s.reply("unknown", 0, &Value::Null).is_none());
    }

    #[test]
    fn a_reply_with_an_if_answers_when_it_holds() {
        let s = Scenario::parse(&json!({ "replies": { "refresh": [{ "if": "data.building", "data": "model" }, { "if": "ui.step == 'views'", "data": "views" }, { "data": "else" }] } }).to_string()).unwrap();
        let answer = |root: Value| s.reply("refresh", 0, &root).unwrap().result.unwrap();
        assert_eq!(answer(json!({ "data": { "building": true } })), "model");
        assert_eq!(answer(json!({ "data": {}, "ui": { "step": "views" } })), "views");
        assert_eq!(answer(json!({ "data": {} })), "else");
    }

    #[test]
    fn merging_lays_objects_over_and_replaces_the_rest() {
        let mut base = json!({ "a": { "x": 1, "y": 2 }, "list": [1, 2], "keep": true });
        merge(&mut base, &json!({ "a": { "y": 3 }, "list": [9] }));
        assert_eq!(base, json!({ "a": { "x": 1, "y": 3 }, "list": [9], "keep": true }));
    }
}
