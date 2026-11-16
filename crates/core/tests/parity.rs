//! Checks the launcher logic against fixed regression vectors.

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::path::PathBuf;

use deskshell_core::launcher::query::{self, Input, Mode};
use deskshell_core::launcher::rank::{self, ActionText, AppFields, AppQuery, AppText, Field};
use deskshell_core::launcher::results::{self, Row};
use deskshell_core::launcher::{bangs, collate, fuzzy, math, quote, text};
use serde_json::{Value, json};

fn vectors(name: &str) -> Value {
    let dir = std::env::var_os("DESKSHELL_PARITY_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/parity/vectors"),
        PathBuf::from,
    );
    let path = dir.join(format!("{name}.json"));
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&source).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[derive(Default)]
struct Check {
    cases: usize,
    failures: Vec<String>,
}

impl Check {
    fn eq<T: PartialEq + Debug + ?Sized>(&mut self, case: impl Fn() -> String, got: &T, want: &T) {
        self.cases += 1;
        if got != want {
            self.failures.push(format!(
                "{}\n    got  {got:?}\n    want {want:?}",
                case()
            ));
        }
    }

    fn finish(self, name: &str) {
        assert!(self.cases > 0, "{name}: no cases");
        let shown: Vec<&str> = self
            .failures
            .iter()
            .take(40)
            .map(String::as_str)
            .collect();
        assert!(
            self.failures.is_empty(),
            "{name}: {} of {} cases differ\n{}",
            self.failures.len(),
            self.cases,
            shown.join("\n")
        );
    }
}

fn rows<'a>(vectors: &'a Value, key: &str) -> &'a [Value] {
    vectors[key]
        .as_array()
        .map_or(&[], Vec::as_slice)
}

fn str_of(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("not a string: {value}"))
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn index_of(value: &Value) -> Option<usize> {
    value
        .as_i64()
        .and_then(|n| usize::try_from(n).ok())
}

fn score_of(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|&score| score >= 0.0)
}

fn non_empty(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
}

fn pair(value: &Value) -> Option<(String, String)> {
    let items = value.as_array()?;
    Some((
        str_of(&items[0]).to_owned(),
        str_of(&items[1]).to_owned(),
    ))
}

#[test]
fn text_primitives() {
    let v = vectors("text");
    let mut check = Check::default();
    let space: HashSet<u64> = rows(&v, "space")
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    for c in (0..=0x10ffff).filter_map(char::from_u32) {
        check.eq(
            || format!("is_js_space U+{:04X}", u32::from(c)),
            &text::is_js_space(c),
            &space.contains(&u64::from(c)),
        );
    }
    for row in rows(&v, "lowercase") {
        check.eq(
            || format!("toLowerCase {row}"),
            &str_of(&row[0]).to_lowercase(),
            &str_of(&row[1]).to_owned(),
        );
    }
    for row in rows(&v, "trim") {
        check.eq(
            || format!("trim {row}"),
            &text::trim(str_of(&row[0])),
            &str_of(&row[1]),
        );
    }
    for row in rows(&v, "length") {
        check.eq(
            || format!("length {row}"),
            &text::utf16_len(str_of(&row[0])),
            &index_of(&row[1]).unwrap(),
        );
    }
    check.finish("text");
}

#[test]
fn math_calculate() {
    let v = vectors("math");
    let mut check = Check::default();
    for row in rows(&v, "calculate") {
        check.eq(
            || format!("calculate {row}"),
            &math::calculate(str_of(&row[0])).as_deref(),
            &non_empty(&row[1]),
        );
    }
    for row in rows(&v, "numbers") {
        let shortest = str_of(&row[0]);
        let value: f64 = shortest.parse().unwrap();
        check.eq(
            || format!("String {row}"),
            &ryu_js::Buffer::new().format(value),
            &shortest,
        );
        check.eq(
            || format!("toPrecision(12) {row}"),
            &ryu_js::Buffer::new().format(math::to_precision_12(value)),
            &str_of(&row[1]),
        );
    }
    check.finish("math");
}

#[test]
fn fuzzy_functions() {
    let v = vectors("fuzzy");
    let mut check = Check::default();
    for row in rows(&v, "isWordStart") {
        let index = index_of(&row[1]).unwrap();
        check.eq(
            || format!("isWordStart {row}"),
            &fuzzy::is_word_start(&units(str_of(&row[0])), index),
            &row[2].as_bool().unwrap(),
        );
    }
    for row in rows(&v, "subsequenceBreaks") {
        check.eq(
            || format!("subsequenceBreaks {row}"),
            &fuzzy::subsequence_breaks(
                &units(str_of(&row[0])),
                &units(str_of(&row[1])),
                row[2].as_bool().unwrap(),
            ),
            &index_of(&row[3]),
        );
    }
    for row in rows(&v, "editDistance") {
        check.eq(
            || format!("editDistance {row}"),
            &fuzzy::edit_distance(
                &units(str_of(&row[0])),
                &units(str_of(&row[1])),
                index_of(&row[2]).unwrap(),
            ),
            &index_of(&row[3]).unwrap(),
        );
    }
    for row in rows(&v, "typoDistance") {
        check.eq(
            || format!("typoDistance {row}"),
            &fuzzy::typo_distance(&units(str_of(&row[0])), &units(str_of(&row[1]))),
            &index_of(&row[2]),
        );
    }
    let mut matchers: HashMap<&str, fuzzy::Matcher> = HashMap::new();
    for row in rows(&v, "score") {
        let (text, query) = (str_of(&row[0]), str_of(&row[1]));
        check.eq(
            || format!("score {row}"),
            &fuzzy::score(text, query),
            &score_of(&row[2]),
        );
        let matcher = matchers
            .entry(query)
            .or_insert_with(|| fuzzy::Matcher::new(query));
        check.eq(
            || format!("Matcher::score {row}"),
            &matcher.score(text),
            &score_of(&row[2]),
        );
    }
    check.finish("fuzzy");
}

#[test]
fn bang_functions() {
    let v = vectors("bangs");
    let mut check = Check::default();
    let parsed = bangs::parse(str_of(&v["source"])).unwrap();
    let want = rows(&v, "parsed");
    check.eq(
        || "parsed count".to_owned(),
        &parsed.len(),
        &want.len(),
    );
    for (bang, row) in parsed.iter().zip(want) {
        let got = json!({
            "t": bang.trigger, "s": bang.site, "d": bang.domain, "sc": bang.subcategory, "symbol": bang.symbol,
            "rank": bang.rank, "name": bang.name, "words": bang.words, "haystack": bang.haystack,
        });
        check.eq(|| format!("parse {row}"), &got, row);
    }
    for row in rows(&v, "symbols") {
        check.eq(
            || format!("symbol {row}"),
            &bangs::category_symbol(row[0].as_str()),
            &str_of(&row[1]),
        );
    }
    for row in rows(&v, "tier") {
        let query = str_of(&row[0]);
        let terms: Vec<&str> = row[1]
            .as_array()
            .unwrap()
            .iter()
            .map(str_of)
            .collect();
        for (bang, want) in parsed
            .iter()
            .zip(row[2].as_array().unwrap())
        {
            check.eq(
                || format!("tier {} {row}", bang.trigger),
                &bangs::tier(bang, query, &terms).map(usize::from),
                &index_of(want),
            );
        }
    }
    for row in rows(&v, "search") {
        let uses = row[1].as_object().unwrap();
        let count = |trigger: &str| {
            uses.get(trigger)
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        let limit = index_of(&row[2]).unwrap();
        let got: Vec<usize> = bangs::search(&parsed, str_of(&row[0]), count, limit)
            .iter()
            .map(|b| b.rank)
            .collect();
        let want: Vec<usize> = row[3]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(index_of)
            .collect();
        check.eq(|| format!("search {row}"), &got, &want);
    }
    for row in rows(&v, "find") {
        check.eq(
            || format!("find {row}"),
            &bangs::find(&parsed, str_of(&row[0])).map(|b| b.rank),
            &index_of(&row[1]),
        );
    }
    check.finish("bangs");
}

struct Fixture<'a> {
    raw: &'a [Value],
    texts: Vec<AppText>,
    launches: HashMap<&'a str, u64>,
}

impl<'a> Fixture<'a> {
    fn new(v: &'a Value) -> Self {
        let raw = rows(v, "apps");
        let list = |app: &'a Value, key: &str| -> Vec<&'a str> {
            app[key]
                .as_array()
                .map(|items| items.iter().map(str_of).collect())
                .unwrap_or_default()
        };
        let texts = raw
            .iter()
            .map(|app| {
                let keywords = list(app, "keywords");
                let categories = list(app, "categories");
                AppText::new(&AppFields {
                    id: field(app, "id"),
                    name: field(app, "name"),
                    generic_name: field(app, "genericName"),
                    comment: field(app, "comment"),
                    exec: field(app, "execString"),
                    startup_class: field(app, "startupClass"),
                    keywords: &keywords,
                    categories: &categories,
                    terminal: app["runInTerminal"]
                        .as_bool()
                        .unwrap_or(false),
                })
            })
            .collect();
        let launches = v["launches"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(id, count)| (id.as_str(), count.as_u64().unwrap()))
            .collect();
        Self { raw, texts, launches }
    }

    fn launches(&self) -> impl Fn(&str) -> u64 + '_ {
        |id| {
            self.launches
                .get(id)
                .copied()
                .unwrap_or(0)
        }
    }

    fn raw_field(&self, index: usize, field_of: Field) -> String {
        let app = &self.raw[index];
        let joined = |key: &str| {
            app[key]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(str_of)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        };
        match field_of {
            Field::Categories => joined("categories"),
            Field::Comment => field(app, "comment").to_owned(),
            Field::Exec => field(app, "execString").to_owned(),
            Field::GenericName => field(app, "genericName").to_owned(),
            Field::Id => field(app, "id").to_owned(),
            Field::Keywords => joined("keywords"),
            Field::StartupClass => field(app, "startupClass").to_owned(),
        }
    }

    fn row(&self, index: usize, described: Option<&str>) -> Value {
        let app = &self.raw[index];
        let id = field(app, "id");
        let description = rank::description(
            described,
            field(app, "genericName"),
            field(app, "comment"),
            id,
        );
        json!([id, description, rank::icon(field(app, "icon"))])
    }

    fn ranked(&self, query: &str, filter: Option<&query::Filter>) -> Vec<Value> {
        let mut prepared = AppQuery::new(query);
        let order = match filter {
            Some(filter) => rank::rank_apps(&self.texts, self.launches(), |app| {
                rank::filter_score(app, filter.field, &mut prepared)
            }),
            None => rank::rank_apps(&self.texts, self.launches(), |app| {
                rank::app_score(app, &mut prepared)
            }),
        };
        order
            .into_iter()
            .map(|index| {
                let described = filter
                    .and_then(|filter| filter.field)
                    .map(|f| self.raw_field(index, f));
                self.row(index, described.as_deref())
            })
            .collect()
    }
}

fn field<'a>(app: &'a Value, key: &str) -> &'a str {
    app[key].as_str().unwrap_or("")
}

fn action_texts(v: &Value) -> (Vec<&str>, Vec<ActionText>) {
    rows(v, "actions")
        .iter()
        .map(|action| {
            (
                str_of(&action[0]),
                ActionText::new(str_of(&action[1]), str_of(&action[2])),
            )
        })
        .unzip()
}

fn ranked_actions(ids: &[&str], actions: &[ActionText], input: &str) -> Vec<Value> {
    let command = text::trim(input);
    let mut results: Vec<Value> = rank::rank_actions(actions, &command.to_lowercase())
        .into_iter()
        .map(|index| json!(["action", ids[index]]))
        .collect();
    if !command.is_empty() {
        results.push(json!(["run", command]));
    }
    results
}

#[test]
fn rank_functions() {
    let v = vectors("rank");
    let mut check = Check::default();
    let fixture = Fixture::new(&v);
    for row in rows(&v, "fieldScore") {
        check.eq(
            || format!("fieldScore {row}"),
            &rank::field_score(&str_of(&row[0]).to_lowercase(), str_of(&row[1])).map(usize::from),
            &index_of(&row[2]),
        );
    }
    for row in rows(&v, "appScore") {
        let index = index_of(&row[0]).unwrap();
        let mut prepared = AppQuery::new(str_of(&row[1]));
        check.eq(
            || format!("appScore {} {row}", fixture.raw[index]["id"]),
            &rank::app_score(&fixture.texts[index], &mut prepared),
            &score_of(&row[2]),
        );
    }
    for row in rows(&v, "rankApps") {
        check.eq(
            || format!("rankApps {}", row[0]),
            &fixture.ranked(str_of(&row[0]), None),
            &row[1].as_array().unwrap().clone(),
        );
    }
    for row in rows(&v, "filterResults") {
        let launcher_query = format!("@{} {}", str_of(&row[0]), str_of(&row[1]));
        let (filter, rest) = query::active_filter(&launcher_query).unwrap();
        check.eq(
            || format!("filterResults {} {}", row[0], row[1]),
            &fixture.ranked(&text::normalize(rest), Some(filter)),
            &row[2].as_array().unwrap().clone(),
        );
    }
    for row in rows(&v, "actionScore") {
        let action = ActionText::new(str_of(&row[0]), str_of(&row[1]));
        check.eq(
            || format!("actionScore {row}"),
            &rank::action_score(&action, str_of(&row[2])).map(usize::from),
            &index_of(&row[3]),
        );
    }
    let (ids, actions) = action_texts(&v);
    for row in rows(&v, "actionResults") {
        check.eq(
            || format!("actionResults {}", row[0]),
            &ranked_actions(&ids, &actions, str_of(&row[0])),
            &row[1].as_array().unwrap().clone(),
        );
    }
    if let [input, sorted] = rows(&v, "collate") {
        let mut names: Vec<&str> = input
            .as_array()
            .unwrap()
            .iter()
            .map(str_of)
            .collect();
        names.sort_by(|left, right| collate::compare(left, right));
        let want: Vec<&str> = sorted
            .as_array()
            .unwrap()
            .iter()
            .map(str_of)
            .collect();
        check.eq(|| "localeCompare sort".to_owned(), &names, &want);
    }
    for row in rows(&v, "compare") {
        check.eq(
            || format!("localeCompare {row}"),
            &(collate::compare(str_of(&row[0]), str_of(&row[1])) as i64),
            &row[2].as_i64().unwrap(),
        );
    }
    check.finish("rank");
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Apps => "apps",
        Mode::Commands => "commands",
        Mode::Filters => "filters",
        Mode::Bangs => "bangs",
    }
}

fn mode_named(name: &str) -> Mode {
    match name {
        "commands" => Mode::Commands,
        "filters" => Mode::Filters,
        "bangs" => Mode::Bangs,
        _ => Mode::Apps,
    }
}

struct Launcher<'a> {
    fixture: Fixture<'a>,
    bangs: Vec<bangs::Bang>,
    uses: HashMap<&'a str, u64>,
    ids: Vec<&'a str>,
    actions: Vec<ActionText>,
}

impl Launcher<'_> {
    fn bang_uses(&self) -> impl Fn(&str) -> u64 + '_ {
        |trigger| {
            self.uses
                .get(trigger)
                .copied()
                .unwrap_or(0)
        }
    }

    fn results(&self, input: &Input, qalc: (&str, &str)) -> Vec<Value> {
        let sources = results::Sources {
            apps: &self.fixture.texts,
            launches: self.fixture.launches(),
            actions: &self.actions,
            bangs: &self.bangs,
            bang_uses: self.bang_uses(),
            qalc_expression: qalc.0,
            qalc_result: qalc.1,
        };
        let built = results::build(input, &sources);
        let field = built
            .filter
            .and_then(|filter| filter.field);
        built
            .rows
            .iter()
            .map(|row| match row {
                Row::App(index) => {
                    let described = field.map(|f| self.fixture.raw_field(*index, f));
                    let mut tagged = vec![json!("app")];
                    tagged.extend(
                        self.fixture
                            .row(*index, described.as_deref())
                            .as_array()
                            .unwrap()
                            .iter()
                            .cloned(),
                    );
                    Value::Array(tagged)
                }
                Row::Filter(filter) => json!(["filter", filter.key.to_string()]),
                Row::Action(index) => json!(["action", self.ids[*index]]),
                Row::Run(command) => json!(["run", command]),
                Row::Calculator { answer, expression } => {
                    json!(["calculator", answer, format!("{expression} · Enter to copy")])
                }
                Row::Bang(bang) => json!(["bang", bang.rank]),
                Row::BangSearch { bang, query } => json!(["bang-search", bang.rank, query]),
                Row::Web(query) => json!(["web", query]),
            })
            .collect()
    }
}

#[test]
fn query_functions() {
    let v = vectors("query");
    let mut check = Check::default();
    let (ids, actions) = action_texts(&v);
    let launcher = Launcher {
        fixture: Fixture::new(&v),
        bangs: bangs::parse(str_of(&v["bangSource"])).unwrap(),
        uses: v["bangUses"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(trigger, count)| (trigger.as_str(), count.as_u64().unwrap()))
            .collect(),
        ids,
        actions,
    };

    for sequence in rows(&v, "sequences") {
        let steps = sequence["steps"].as_array().unwrap();
        let states = sequence["states"].as_array().unwrap();
        let mut input = Input::default();
        let mut qalc = (String::new(), String::new());
        for (number, (step, want)) in steps.iter().zip(states).enumerate() {
            let mut step_back = Value::Null;
            if let Some(typed) = step.as_str() {
                if typed != input.text() {
                    input.handle_input(typed);
                }
            } else if step.get("back").is_some() {
                step_back = Value::Bool(input.step_back());
            } else if let Some(mode) = step.get("mode") {
                input.set_mode(mode_named(str_of(mode)));
            } else if step.get("open").is_some() {
                input.set_mode(Mode::Apps);
            } else if let Some(key) = step.get("filter") {
                input.use_filter(str_of(key).chars().next().unwrap());
            } else if let Some(trigger) = step.get("bang") {
                let bang = bangs::find(&launcher.bangs, str_of(trigger)).unwrap();
                input.use_bang(&bang.trigger);
            } else if let Some((expression, result)) = step.get("qalc").and_then(pair) {
                qalc = (expression, result);
            }
            let launcher_query = input.launcher_query();
            let got = json!({
                "stepBack": step_back,
                "mode": mode_name(input.mode()),
                "filterKey": input.filter_key().map(String::from).unwrap_or_default(),
                "bang": input.bang(),
                "text": input.text(),
                "field": input.text(),
                "launcherQuery": launcher_query,
                "calcExpression": query::calc_expression(&launcher_query).unwrap_or(""),
                "results": launcher.results(&input, (&qalc.0, &qalc.1)),
            });
            check.eq(
                || {
                    format!(
                        "sequence {} step {number} {step}",
                        sequence["steps"]
                    )
                },
                &got,
                want,
            );
        }
    }

    for row in rows(&v, "launcherQuery") {
        let launcher_query = str_of(&row[0]);
        let mode = query::mode_of(launcher_query);
        let split = |found: Option<(char, &str)>| {
            found.map(|(key, rest)| (key.to_string(), rest.to_owned()))
        };
        let got = (
            mode == Mode::Commands,
            mode == Mode::Filters,
            mode == Mode::Bangs,
            split(
                launcher_query
                    .strip_prefix('@')
                    .and_then(query::split_key),
            ),
            query::active_filter(launcher_query).map(|(filter, _)| filter.key.to_string()),
            query::calc_expression(launcher_query),
            split(query::split_key(launcher_query)),
            query::bang_shortcut(launcher_query)
                .map(|(trigger, rest)| (trigger.to_owned(), rest.to_owned())),
        );
        let want = (
            row[1].as_bool().unwrap(),
            row[2].as_bool().unwrap(),
            row[3].as_bool().unwrap(),
            pair(&row[4]),
            row[5].as_str().map(str::to_owned),
            non_empty(&row[6]),
            pair(&row[7]),
            pair(&row[8]),
        );
        check.eq(|| format!("launcherQuery {row}"), &got, &want);
    }

    for row in rows(&v, "calculatorAnswer") {
        check.eq(
            || format!("calculatorAnswer {row}"),
            &query::calculator_answer(str_of(&row[0]), str_of(&row[1]), str_of(&row[2])).as_deref(),
            &non_empty(&row[3]),
        );
    }
    check.finish("query");
}

#[test]
fn quote_functions() {
    let v = vectors("quote");
    let mut check = Check::default();
    for row in rows(&v, "lua") {
        check.eq(
            || format!("luaString {row}"),
            &quote::lua_string(str_of(&row[0])),
            &str_of(&row[1]).to_owned(),
        );
    }
    for row in rows(&v, "launch") {
        let command: Vec<&str> = row[0]
            .as_array()
            .unwrap()
            .iter()
            .map(str_of)
            .collect();
        let tenant: Vec<&str> = row[2]
            .as_array()
            .map(|items| items.iter().map(str_of).collect())
            .unwrap_or_default();
        check.eq(
            || format!("launchFloating {row}"),
            &quote::float_dispatch(
                &quote::shell_command(&command, str_of(&row[1])),
                &tenant,
            ),
            &str_of(&row[3]).to_owned(),
        );
    }
    check.finish("quote");
}
