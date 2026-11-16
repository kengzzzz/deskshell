//! The rows the launcher lists for its input.

use std::borrow::Cow;

use super::bangs::{self, Bang};
use super::query::{self, Filter, Input, Mode};
use super::rank::{self, ActionText, AppQuery, AppText};
use super::text::{normalize, trim};

pub const BANG_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub enum Row<'a> {
    App(usize),
    Filter(&'static Filter),
    Action(usize),
    Run(&'a str),
    Calculator {
        answer: Cow<'a, str>,
        /// The expression the row's description shows.
        expression: &'a str,
    },
    Bang(&'a Bang),
    BangSearch {
        bang: &'a Bang,
        /// Empty opens the site.
        query: &'a str,
    },
    Web(Cow<'a, str>),
}

#[derive(Debug, Clone, Copy)]
pub struct Sources<'a, L, U> {
    pub apps: &'a [AppText],
    pub launches: L,
    pub actions: &'a [ActionText],
    pub bangs: &'a [Bang],
    pub bang_uses: U,
    pub qalc_expression: &'a str,
    pub qalc_result: &'a str,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Results<'a> {
    pub rows: Vec<Row<'a>>,
    pub filter: Option<&'static Filter>,
}

pub fn build<'a, L, U>(input: &'a Input, sources: &Sources<'a, L, U>) -> Results<'a>
where
    L: Fn(&str) -> u64,
    U: Fn(&str) -> u64,
{
    match input.mode() {
        Mode::Commands => Results { rows: commands(input.text(), sources), filter: None },
        Mode::Filters => filters(input, sources),
        Mode::Bangs => Results { rows: bang_rows(input, sources), filter: None },
        Mode::Apps => Results { rows: apps(input.text(), sources), filter: None },
    }
}

fn commands<'a, L, U>(text: &'a str, sources: &Sources<'a, L, U>) -> Vec<Row<'a>> {
    let command = trim(text);
    let order = rank::rank_actions(sources.actions, &command.to_lowercase());
    let mut rows = Vec::with_capacity(order.len() + 1);
    rows.extend(order.into_iter().map(Row::Action));
    if !command.is_empty() {
        rows.push(Row::Run(command));
    }
    rows
}

fn filters<'a, L, U>(input: &'a Input, sources: &Sources<'a, L, U>) -> Results<'a>
where
    L: Fn(&str) -> u64,
{
    let launcher_query = input.launcher_query();
    match query::active_filter(&launcher_query) {
        Some((filter, rest)) => {
            let normalized = normalize(rest);
            let mut prepared = AppQuery::new(&normalized);
            let order = rank::rank_apps(sources.apps, &sources.launches, |app| {
                rank::filter_score(app, filter.field, &mut prepared)
            });
            Results {
                rows: order
                    .into_iter()
                    .map(Row::App)
                    .collect(),
                filter: Some(filter),
            }
        }
        None => Results {
            rows: query::matching_filters(&launcher_query)
                .map(Row::Filter)
                .collect(),
            filter: None,
        },
    }
}

fn bang_rows<'a, L, U>(input: &'a Input, sources: &Sources<'a, L, U>) -> Vec<Row<'a>>
where
    U: Fn(&str) -> u64,
{
    let typed = trim(input.text());
    if let Some(trigger) = input.bang() {
        return bangs::find(sources.bangs, trigger)
            .map(|bang| Row::BangSearch { bang, query: typed })
            .into_iter()
            .collect();
    }
    let found = bangs::search(
        sources.bangs,
        typed,
        &sources.bang_uses,
        BANG_LIMIT,
    );
    let mut rows = Vec::with_capacity(found.len() + 1);
    // "yt cats" offers the YouTube search first, without hiding bangs that match both words.
    if let Some((trigger, search)) = query::bang_shortcut(typed)
        && let Some(bang) = bangs::find(sources.bangs, trigger)
    {
        rows.push(Row::BangSearch { bang, query: search });
    }
    rows.extend(found.into_iter().map(Row::Bang));
    if rows.is_empty() && !typed.is_empty() {
        rows.push(Row::Web(Cow::Owned(format!("!{typed}"))));
    }
    rows
}

fn apps<'a, L, U>(text: &'a str, sources: &Sources<'a, L, U>) -> Vec<Row<'a>>
where
    L: Fn(&str) -> u64,
{
    let normalized = normalize(text);
    let mut prepared = AppQuery::new(&normalized);
    let order = rank::rank_apps(sources.apps, &sources.launches, |app| {
        rank::app_score(app, &mut prepared)
    });
    if normalized.is_empty() {
        return order
            .into_iter()
            .map(Row::App)
            .collect();
    }
    let trimmed = trim(text);
    let mut rows = Vec::with_capacity(order.len() + 2);
    if let Some(answer) =
        query::calculator_answer(text, sources.qalc_expression, sources.qalc_result)
    {
        rows.push(Row::Calculator {
            answer,
            expression: query::calc_expression(text).unwrap_or(trimmed),
        });
    }
    rows.extend(order.into_iter().map(Row::App));
    rows.push(Row::Web(Cow::Borrowed(trimmed)));
    rows
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;
    use crate::launcher::rank::AppFields;

    type Count = fn(&str) -> u64;

    fn sources<'a>(
        apps: &'a [AppText],
        actions: &'a [ActionText],
        bangs: &'a [Bang],
    ) -> Sources<'a, Count, Count> {
        Sources {
            apps,
            launches: |_| 0,
            actions,
            bangs,
            bang_uses: |_| 0,
            qalc_expression: "",
            qalc_result: "",
        }
    }

    fn typed(steps: &[&str]) -> Input {
        let mut input = Input::default();
        for step in steps {
            input.handle_input(step);
        }
        input
    }

    #[test]
    fn rows_per_mode() {
        let apps = [AppText::new(&AppFields { id: "foot", name: "Foot", ..AppFields::default() })];
        let actions = [ActionText::new("Clipboard", "paste history")];
        let bangs = bangs::parse(r#"[{"t": "yt", "s": "YouTube", "r": 9}]"#).unwrap();
        let sources = sources(&apps, &actions, &bangs);

        let input = typed(&[""]);
        assert_eq!(build(&input, &sources).rows, [Row::App(0)]);

        let input = typed(&["2+2"]);
        let rows = build(&input, &sources).rows;
        assert_eq!(
            rows,
            [
                Row::Calculator { answer: Cow::Borrowed("4"), expression: "2+2" },
                Row::Web(Cow::Borrowed("2+2")),
            ]
        );

        let input = typed(&[">clip "]);
        assert_eq!(
            build(&input, &sources).rows,
            [Row::Action(0), Row::Run("clip")]
        );

        let input = typed(&["!", "yt cats"]);
        assert_eq!(
            build(&input, &sources).rows,
            [Row::BangSearch { bang: &bangs[0], query: "cats" }]
        );
        let input = typed(&["!", "zz"]);
        assert_eq!(
            build(&input, &sources).rows,
            [Row::Web(Cow::Owned(String::from("!zz")))]
        );

        let input = typed(&["@", "t"]);
        let results = build(&input, &sources);
        assert_eq!(results.filter, None);
        assert_matches!(results.rows[..], [Row::Filter(filter)] if filter.key == 't');
        let input = typed(&["@", "i fo"]);
        let results = build(&input, &sources);
        assert_eq!(results.filter.map(|filter| filter.key), Some('i'));
        assert_eq!(results.rows, [Row::App(0)]);
    }
}
