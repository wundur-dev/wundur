//! A play link's stats by day, in this Mac's time zone: the API counts by UTC hour
//! ([`wundur_core::play_link::Stats`]), and the hours are added up into local days here. Drawn as
//! a bar chart of views and players per day, and a table of every day with its play time and
//! ratings.

use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_core::play_link::{STATS_START, Stats};

/// One local day of a link's activity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Day {
    pub date: NaiveDate,
    pub views: u64,
    pub players: u64,
    pub play_seconds: u64,
    pub plays_timed: u64,
    pub rating_sum: u64,
    pub ratings: u64,
}

impl Day {
    fn avg_play(&self) -> Option<f64> {
        (self.plays_timed > 0).then(|| self.play_seconds as f64 / self.plays_timed as f64)
    }

    fn rating(&self) -> Option<f64> {
        (self.ratings > 0).then(|| self.rating_sum as f64 / self.ratings as f64)
    }
}

/// A UTC hour from the API (`2026-11-23T19`).
fn parse_hour(h: &str) -> Option<chrono::DateTime<Utc>> {
    NaiveDateTime::parse_from_str(&format!("{h}:00"), "%Y-%m-%dT%H:%M").ok().map(|t| Utc.from_utc_datetime(&t))
}

/// The link's days in `tz`, oldest first, from the day it was published (or the day stats by
/// hour began, if later) to `today`, days with nothing included. The flag: it was published
/// before stats by hour began, so its first days aren't here.
pub fn days<Tz: TimeZone>(stats: &Stats, tz: &Tz, today: NaiveDate) -> (Vec<Day>, bool) {
    let start_hour = parse_hour(STATS_START);
    let since = stats.since.as_deref().and_then(parse_hour);
    let started_late = matches!((since, start_hour), (Some(s), Some(b)) if s < b);
    let local = |t: chrono::DateTime<Utc>| t.with_timezone(tz).date_naive();
    let first_hour = stats.hours.first().and_then(|h| parse_hour(&h.hour));
    let from = [since.map(|s| start_hour.map_or(s, |b| s.max(b))), first_hour].into_iter().flatten().min().map(local);
    let Some(from) = from else { return (Vec::new(), started_late) };
    let from = from.min(today);
    let mut out: Vec<Day> = from.iter_days().take_while(|d| *d <= today).map(|date| Day { date, ..Default::default() }).collect();
    for h in &stats.hours {
        let Some(t) = parse_hour(&h.hour) else { continue };
        let date = local(t);
        let Some(day) = out.iter_mut().find(|d| d.date == date) else { continue };
        day.views += h.views;
        day.players += h.players;
        day.play_seconds += h.play_seconds;
        day.plays_timed += h.plays_timed;
        day.rating_sum += h.rating_sum;
        day.ratings += h.ratings;
    }
    (out, started_late)
}

/// The series' colours: views and players, validated together for both modes (colour-blind
/// safe, 3:1 on the surface).
fn colors(dark: bool) -> (Hsla, Hsla) {
    if dark { (rgb(0x3987e5).into(), rgb(0xd95926).into()) } else { (rgb(0x2a78d6).into(), rgb(0xeb6834).into()) }
}

/// The chart's height for the tallest bar.
const PLOT: f32 = 132.;

/// The chart and the table.
pub fn render(days: &[Day], started_late: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (fg, muted, border) = (theme.foreground, theme.muted_foreground, theme.border);
    let (views_color, players_color) = colors(theme.mode.is_dark());
    let zone = zone_label(chrono::Local::now().offset().local_minus_utc());
    if days.is_empty() {
        return div().text_xs().text_color(muted).child("No activity yet. Stats show here once people open the link.").into_any_element();
    }
    let max = days.iter().map(|d| d.views.max(d.players)).max().unwrap_or(0).max(1);
    let n = days.len();
    // Every day labelled up to two weeks; past that, every few days.
    let every = n.div_ceil(14).max(1);
    let legend = |color: Hsla, label: &'static str| div().flex().items_center().gap_1().child(div().size(px(9.)).rounded_sm().bg(color)).child(div().text_xs().text_color(fg).child(label));
    let columns = days.iter().enumerate().map(|(i, d)| {
        let bar = |value: u64, color: Hsla| {
            let h = if value == 0 { 0. } else { (value as f32 / max as f32 * PLOT).max(2.) };
            div().flex_1().max_w(px(14.)).h(px(h)).rounded_tl(px(4.)).rounded_tr(px(4.)).bg(color)
        };
        let tip = tooltip_text(d);
        let label = if i % every == 0 || i == n - 1 {
            if n <= 7 { d.date.format("%a %-d").to_string() } else { d.date.format("%-d").to_string() }
        } else {
            String::new()
        };
        div()
            .id(("play-day", i))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .rounded_sm()
            .hover(|s| s.bg(border.opacity(0.35)))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
            .child(div().flex_none().w_full().h(px(PLOT)).flex().items_end().justify_center().gap(px(2.)).px(px(1.)).child(bar(d.views, views_color)).child(bar(d.players, players_color)))
            .child(div().pt_1().text_xs().text_color(muted).child(label))
    });
    let chart = div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(legend(views_color, "Views"))
                .child(legend(players_color, "Players"))
                .child(div().flex_1())
                .child(div().text_xs().text_color(muted).child(format!("Days in your time zone ({zone})"))),
        )
        .child(
            div()
                .flex()
                .gap_1()
                // The scale: the tallest bar's value at the top line, 0 at the baseline.
                .child(
                    div()
                        .flex_none()
                        .w(px(28.))
                        .h(px(PLOT))
                        .flex()
                        .flex_col()
                        .justify_between()
                        .items_end()
                        .text_xs()
                        .text_color(muted)
                        .child(div().mt(px(-7.)).child(max.to_string()))
                        .child(div().mb(px(-7.)).child("0")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .relative()
                        .child(div().absolute().top_0().left_0().right_0().border_t_1().border_color(border.opacity(0.6)))
                        .child(div().absolute().top(px(PLOT)).left_0().right_0().border_t_1().border_color(border))
                        .child(div().flex().items_start().children(columns)),
                ),
        );
    let cell = |text: String, right: bool| div().w(px(86.)).flex_none().when(right, |d| d.text_right()).child(text);
    let header = div()
        .flex()
        .gap_2()
        .py_1()
        .border_b_1()
        .border_color(border)
        .text_xs()
        .text_color(muted)
        .child(div().flex_1().child("Day"))
        .child(cell("Views".into(), true))
        .child(cell("Players".into(), true))
        .child(cell("Avg. play".into(), true))
        .child(cell("Rating".into(), true));
    let rows = days.iter().rev().map(|d| {
        div()
            .flex()
            .gap_2()
            .py_1()
            .border_b_1()
            .border_color(border.opacity(0.5))
            .text_xs()
            .text_color(fg)
            .child(div().flex_1().child(d.date.format("%a, %b %-d").to_string()))
            .child(cell(d.views.to_string(), true))
            .child(cell(d.players.to_string(), true))
            .child(cell(d.avg_play().map_or_else(|| "–".into(), crate::play_link_sheet::duration), true))
            .child(cell(d.rating().map_or_else(|| "–".into(), |r| format!("★ {r:.1} ({})", d.ratings)), true))
    });
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(chart)
        .child(div().flex().flex_col().child(header).children(rows))
        .when(started_late, |d| {
            d.child(div().text_xs().text_color(muted).child(format!(
                "Stats by day start {}. Earlier activity is only in the totals.",
                parse_hour(STATS_START).map(|t| t.with_timezone(&chrono::Local).format("%b %-d").to_string()).unwrap_or_default()
            )))
        })
        .child(div().text_xs().text_color(muted).child("Views and players are people, counted once a day. Ratings are only shown to you."))
        .into_any_element()
}

/// A UTC offset as people write it: "UTC", "UTC−5", "UTC+5:30".
fn zone_label(secs: i32) -> String {
    if secs == 0 {
        return "UTC".into();
    }
    let sign = if secs < 0 { '−' } else { '+' };
    let (h, m) = (secs.abs() / 3600, (secs.abs() % 3600) / 60);
    if m == 0 { format!("UTC{sign}{h}") } else { format!("UTC{sign}{h}:{m:02}") }
}

fn tooltip_text(d: &Day) -> String {
    let mut lines = vec![d.date.format("%A, %b %-d").to_string(), format!("{} · {}", count(d.views, "view"), count(d.players, "player"))];
    if let Some(a) = d.avg_play() {
        lines.push(format!("Avg. play {}", crate::play_link_sheet::duration(a)));
    }
    if let Some(r) = d.rating() {
        lines.push(format!("★ {r:.1} from {}", count(d.ratings, "rating")));
    }
    lines.join("\n")
}

fn count(n: u64, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

#[cfg(test)]
mod tests {
    use super::{Day, days, tooltip_text, zone_label};
    use chrono::{FixedOffset, NaiveDate};
    use wundur_core::play_link::{HourStats, Stats};

    fn hour(h: &str, views: u64, players: u64) -> HourStats {
        HourStats { hour: h.into(), views, players, ..Default::default() }
    }

    #[test]
    fn utc_hours_become_local_days() {
        let stats = Stats {
            since: Some("2026-11-24T15".into()),
            hours: vec![
                hour("2026-11-24T20", 2, 1),
                // 7 pm and later in New York is the next UTC day: still Nov 24 there.
                hour("2026-11-25T01", 3, 2),
                hour("2026-11-26T14", 1, 0),
                HourStats { hour: "2026-11-26T15".into(), views: 1, players: 1, play_seconds: 600, plays_timed: 2, rating_sum: 9, ratings: 2 },
            ],
        };
        let ny = FixedOffset::west_opt(5 * 3600).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        let (d, late) = days(&stats, &ny, today);
        assert!(!late);
        let got: Vec<(u32, u64, u64)> = d.iter().map(|d| (chrono::Datelike::day(&d.date), d.views, d.players)).collect();
        assert_eq!(got, [(24, 5, 3), (25, 0, 0), (26, 2, 1), (27, 0, 0)]);
        assert_eq!((d[2].play_seconds, d[2].plays_timed, d[2].rating_sum, d[2].ratings), (600, 2, 9, 2));
        // The same hours in UTC split differently.
        let utc = FixedOffset::east_opt(0).unwrap();
        let (d, _) = days(&stats, &utc, today);
        assert_eq!((d[0].views, d[1].views), (2, 3));
    }

    #[test]
    fn links_from_before_stats_by_hour_say_so() {
        let stats = Stats { since: Some("2026-11-20T15".into()), hours: vec![hour("2026-11-24T10", 1, 1)] };
        let utc = FixedOffset::east_opt(0).unwrap();
        let (d, late) = days(&stats, &utc, NaiveDate::from_ymd_opt(2026, 11, 25).unwrap());
        assert!(late);
        assert_eq!(d.first().map(|d| d.date), NaiveDate::from_ymd_opt(2026, 11, 24));
        assert_eq!(d.len(), 2);
        let none = days(&Stats::default(), &utc, NaiveDate::from_ymd_opt(2026, 11, 25).unwrap());
        assert!(none.0.is_empty());
    }

    #[test]
    fn zones_read_as_people_write_them() {
        assert_eq!((zone_label(0), zone_label(-5 * 3600), zone_label(19_800)), ("UTC".into(), "UTC−5".into(), "UTC+5:30".to_string()));
    }

    #[test]
    fn tooltips_say_the_day() {
        let d = Day { date: NaiveDate::from_ymd_opt(2026, 11, 23).unwrap(), views: 9, players: 3, play_seconds: 936, plays_timed: 3, rating_sum: 9, ratings: 2 };
        assert_eq!(tooltip_text(&d), "Monday, Nov 23\n9 views · 3 players\nAvg. play 5m 12s\n★ 4.5 from 2 ratings");
    }
}
