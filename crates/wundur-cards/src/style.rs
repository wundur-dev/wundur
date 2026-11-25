//! A node's properties as GPUI styles: sizes in points, colours by the theme's names, and the
//! icons by their Lucide names. A value that can't be read is left out, never an error: the
//! node is drawn without it and `Manifest::check` says what was wrong.

use std::collections::HashMap;
use std::sync::OnceLock;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::ActiveTheme;
use gpui_kit::*;
use serde_json::{Map, Value};

pub type Props = Map<String, Value>;

/// The colours a manifest may name. Each can be thinned: `accent/12` is the accent at 12%.
pub const COLORS: [&str; 12] = ["text", "muted", "accent", "danger", "success", "warning", "border", "surface", "background", "skeleton", "white", "transparent"];

pub fn color(name: &str, cx: &App) -> Option<Hsla> {
    let (name, strength) = match name.split_once('/') {
        Some((name, percent)) => (name, percent.trim().parse::<f32>().ok().map(|p| (p / 100.).clamp(0., 1.))),
        None => (name, None),
    };
    let theme = cx.theme();
    let base = match name.trim() {
        "text" => theme.foreground,
        "muted" => theme.muted_foreground,
        "accent" => theme.ring,
        "danger" => theme.danger,
        "success" => theme.green,
        "warning" => theme.yellow,
        "border" => theme.border,
        "surface" => theme.secondary,
        "background" => theme.background,
        "skeleton" => theme.skeleton,
        "white" => gpui_kit::white(),
        "transparent" => gpui_kit::transparent_black(),
        _ => return None,
    };
    Some(strength.map_or(base, |s| base.opacity(s)))
}

/// A colour property; `true` is the default given.
pub fn color_of(props: &Props, key: &str, default: &str, cx: &App) -> Option<Hsla> {
    match props.get(key)? {
        Value::String(name) => color(name, cx),
        Value::Bool(true) => color(default, cx),
        _ => None,
    }
}

pub fn num(props: &Props, key: &str) -> Option<f32> {
    props.get(key).and_then(Value::as_f64).map(|n| n as f32)
}

pub fn flag(props: &Props, key: &str) -> bool {
    matches!(props.get(key), Some(Value::Bool(true)))
}

/// A length: points, `"fill"`, or a share of the parent (`"50%"`).
pub fn length(value: &Value) -> Option<Length> {
    match value {
        Value::Number(n) => Some(px(n.as_f64()? as f32).into()),
        Value::String(s) if s == "fill" => Some(relative(1.).into()),
        Value::String(s) if s == "auto" => Some(Length::Auto),
        Value::String(s) => s.strip_suffix('%').and_then(|p| p.trim().parse::<f32>().ok()).map(|p| relative(p / 100.).into()),
        _ => None,
    }
}

/// Edges as top, right, bottom, left: one number for all, two for vertical and horizontal,
/// or all four.
pub fn edges(value: &Value) -> Option<[f32; 4]> {
    let n = |v: &Value| v.as_f64().map(|n| n as f32);
    match value {
        Value::Number(_) => n(value).map(|a| [a; 4]),
        Value::Array(items) => match items.as_slice() {
            [v, h] => Some([n(v)?, n(h)?, n(v)?, n(h)?]),
            [t, r, b, l] => Some([n(t)?, n(r)?, n(b)?, n(l)?]),
            _ => None,
        },
        _ => None,
    }
}

/// What any node may say about its own size and place in its parent.
pub const SIZING: [&str; 11] = ["grow", "shrink", "basis", "width", "height", "min_width", "max_width", "min_height", "max_height", "align_self", "opacity"];

pub fn has_sizing(props: &Props) -> bool {
    SIZING.iter().any(|k| props.contains_key(*k))
}

pub fn sized<E: Styled>(mut e: E, props: &Props) -> E {
    if let Some(g) = num(props, "grow") {
        e = e.flex_grow(g);
    }
    if let Some(s) = num(props, "shrink") {
        e = e.flex_shrink(s);
    }
    if let Some(l) = props.get("basis").and_then(length) {
        e = e.flex_basis(l);
    }
    if let Some(l) = props.get("width").and_then(length) {
        e = e.w(l);
    }
    if let Some(l) = props.get("height").and_then(length) {
        e = e.h(l);
    }
    if let Some(l) = props.get("min_width").and_then(length) {
        e = e.min_w(l);
    }
    if let Some(l) = props.get("max_width").and_then(length) {
        e = e.max_w(l);
    }
    if let Some(l) = props.get("min_height").and_then(length) {
        e = e.min_h(l);
    }
    if let Some(l) = props.get("max_height").and_then(length) {
        e = e.max_h(l);
    }
    match props.get("align_self").and_then(Value::as_str) {
        Some("start") => e = e.self_start(),
        Some("center") => e = e.self_center(),
        Some("end") => e = e.self_end(),
        Some("stretch") => e = e.self_stretch(),
        _ => {}
    }
    if let Some(o) = num(props, "opacity") {
        e = e.opacity(o.clamp(0., 1.));
    }
    e
}

/// A box's own look: spacing, alignment, ground, borders, corners.
pub fn boxed<E: Styled>(mut e: E, props: &Props, cx: &App) -> E {
    match props.get("gap") {
        Some(Value::Array(pair)) => {
            if let [x, y] = pair.as_slice() {
                e = e.gap_x(px(x.as_f64().unwrap_or(0.) as f32)).gap_y(px(y.as_f64().unwrap_or(0.) as f32));
            }
        }
        Some(v) => {
            if let Some(g) = v.as_f64() {
                e = e.gap(px(g as f32));
            }
        }
        None => {}
    }
    if let Some([t, r, b, l]) = props.get("padding").and_then(edges) {
        e = e.pt(px(t)).pr(px(r)).pb(px(b)).pl(px(l));
    }
    match props.get("align").and_then(Value::as_str) {
        Some("start") => e = e.items_start(),
        Some("center") => e = e.items_center(),
        Some("end") => e = e.items_end(),
        Some("baseline") => e = e.items_baseline(),
        _ => {}
    }
    match props.get("justify").and_then(Value::as_str) {
        Some("start") => e = e.justify_start(),
        Some("center") => e = e.justify_center(),
        Some("end") => e = e.justify_end(),
        Some("between") => e = e.justify_between(),
        Some("around") => e = e.justify_around(),
        _ => {}
    }
    if flag(props, "wrap") {
        e = e.flex_wrap();
    }
    if let Some(c) = color_of(props, "background", "surface", cx) {
        e = e.bg(c);
    }
    if let Some(c) = color_of(props, "border", "border", cx) {
        e = e.border_1().border_color(c);
    }
    // One side's line: the border colour is the box's one colour, so a box has sides or a
    // whole border, with the last named winning.
    for (key, side) in [("border_top", 0), ("border_bottom", 1), ("border_left", 2), ("border_right", 3)] {
        if let Some(c) = color_of(props, key, "border", cx) {
            e = match side {
                0 => e.border_t_1(),
                1 => e.border_b_1(),
                2 => e.border_l_1(),
                _ => e.border_r_1(),
            }
            .border_color(c);
        }
    }
    if let Some(r) = num(props, "radius") {
        e = e.rounded(px(r));
    }
    if flag(props, "clip") {
        e = e.overflow_hidden();
    }
    if flag(props, "shadow") {
        e = e.shadow_sm();
    }
    if let Some(a) = num(props, "aspect") {
        e = e.aspect_ratio(a);
    }
    e
}

/// A text size: a name, or points.
pub fn text_size<E: Styled>(e: E, value: Option<&Value>) -> E {
    match value {
        Some(Value::String(s)) => match s.as_str() {
            "xs" => e.text_xs(),
            "sm" => e.text_sm(),
            "lg" => e.text_lg(),
            "xl" => e.text_xl(),
            _ => e.text_base(),
        },
        Some(Value::Number(n)) => e.text_size(px(n.as_f64().unwrap_or(14.) as f32)),
        _ => e.text_sm(),
    }
}

pub fn weight(name: Option<&str>) -> Option<FontWeight> {
    Some(match name? {
        "medium" => FontWeight::MEDIUM,
        "semibold" => FontWeight::SEMIBOLD,
        "bold" => FontWeight::BOLD,
        "normal" => FontWeight::NORMAL,
        _ => return None,
    })
}

/// An icon's size: a name, or points.
pub fn icon_size(value: Option<&Value>) -> f32 {
    match value {
        Some(Value::String(s)) => match s.as_str() {
            "xs" => 12.,
            "sm" => 14.,
            "lg" => 20.,
            "xl" => 24.,
            _ => 16.,
        },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(16.) as f32,
        _ => 16.,
    }
}

/// A Lucide icon by its name as lucide.dev writes it (`audio-lines`).
pub fn icon(name: &str) -> Option<Lucide> {
    static BY_NAME: OnceLock<HashMap<String, Lucide>> = OnceLock::new();
    BY_NAME
        .get_or_init(|| {
            Lucide::ALL
                .iter()
                .filter_map(|icon| {
                    let path = icon.path();
                    let name = path.rsplit('/').next()?.strip_suffix(".svg")?.to_string();
                    Some((name, *icon))
                })
                .collect()
        })
        .get(name.trim())
        .copied()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[::core::prelude::v1::test]
    fn icons_are_found_by_their_lucide_names() {
        assert!(icon("audio-lines").is_some());
        assert!(icon("sparkles").is_some());
        assert!(icon("box").is_some());
        assert!(icon("no-such-icon").is_none());
    }

    #[::core::prelude::v1::test]
    fn lengths_and_edges_read_their_forms() {
        assert!(length(&json!(120)).is_some());
        assert!(length(&json!("fill")).is_some());
        assert!(length(&json!("50%")).is_some());
        assert!(length(&json!("wide")).is_none());
        assert_eq!(edges(&json!(8)), Some([8.; 4]));
        assert_eq!(edges(&json!([4, 12])), Some([4., 12., 4., 12.]));
        assert_eq!(edges(&json!([1, 2, 3, 4])), Some([1., 2., 3., 4.]));
        assert_eq!(edges(&json!([1, 2, 3])), None);
    }
}
