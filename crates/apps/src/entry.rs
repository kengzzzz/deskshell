//! Parses one desktop entry file. Keys are not trimmed and only `Exec` is unescaped, on purpose.

use std::collections::HashMap;

use tracing::debug;

use crate::Locale;
use crate::locale::LocaleRef;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub generic_name: String,
    pub comment: String,
    pub icon: String,
    pub exec: String,
    pub command: Vec<String>,
    pub working_directory: String,
    pub terminal: bool,
    pub categories: Vec<String>,
    pub keywords: Vec<String>,
    pub startup_class: String,
    pub no_display: bool,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Action {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub exec: String,
    pub command: Vec<String>,
}

impl Entry {
    pub fn is_valid(&self) -> bool {
        !self.name.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub entry: Entry,
    pub hidden: bool,
}

type Values<'a> = HashMap<&'a str, (LocaleRef<'a>, &'a str)>;

pub fn parse(id: &str, text: &str, system: &Locale) -> Parsed {
    let mut parsed =
        Parsed { entry: Entry { id: id.to_owned(), ..Entry::default() }, hidden: false };
    let mut group = "";
    let mut values: Values<'_> = HashMap::new();
    let mut action_order: Vec<&str> = Vec::new();
    let mut pending: HashMap<&str, Action> = HashMap::new();

    for line in text
        .split('\n')
        .filter(|line| !line.is_empty())
    {
        if line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_circumfix('[', ']') {
            finish_group(
                group,
                &mut values,
                &mut parsed,
                &mut action_order,
                &mut pending,
            );
            group = name;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            debug!(id, line, "desktop entry line without =");
            continue;
        };
        let (key, locale) = match key.find('[') {
            Some(at) if at + 1 != key.len() => {
                let inner = &key[at + 1..];
                let cut = inner
                    .char_indices()
                    .last()
                    .map_or(0, |(last, _)| last);
                (&key[..at], LocaleRef::parse(&inner[..cut]))
            }
            _ => (key, LocaleRef::default()),
        };
        match values.get(key) {
            Some((old, _)) => {
                let old_score = system.score(*old);
                let new_score = system.score(locale);
                if new_score > old_score || (old_score == 0 && !locale.is_valid()) {
                    values.insert(key, (locale, value));
                }
            }
            None => {
                values.insert(key, (locale, value));
            }
        }
    }
    finish_group(
        group,
        &mut values,
        &mut parsed,
        &mut action_order,
        &mut pending,
    );

    for id in action_order {
        if let Some(action) = pending.get(id) {
            parsed
                .entry
                .actions
                .push(action.clone());
        }
    }
    parsed
}

fn finish_group<'a>(
    group: &'a str,
    values: &mut Values<'a>,
    parsed: &mut Parsed,
    action_order: &mut Vec<&'a str>,
    pending: &mut HashMap<&'a str, Action>,
) {
    if group == "Desktop Entry" {
        if values
            .get("Type")
            .is_none_or(|(_, kind)| *kind != "Application")
        {
            return;
        }
        let entry = &mut parsed.entry;
        for (&key, &(_, value)) in values.iter() {
            match key {
                "Name" => value.clone_into(&mut entry.name),
                "GenericName" => value.clone_into(&mut entry.generic_name),
                "StartupWMClass" => value.clone_into(&mut entry.startup_class),
                "NoDisplay" => entry.no_display = value == "true",
                "Hidden" => parsed.hidden = value == "true",
                "Comment" => value.clone_into(&mut entry.comment),
                "Icon" => value.clone_into(&mut entry.icon),
                "Exec" => {
                    value.clone_into(&mut entry.exec);
                    entry.command = parse_exec(value);
                }
                "Path" => value.clone_into(&mut entry.working_directory),
                "Terminal" => entry.terminal = value == "true",
                "Categories" => entry.categories = split_list(value),
                "Keywords" => entry.keywords = split_list(value),
                "Actions" => {
                    *action_order = value
                        .split(';')
                        .filter(|id| !id.is_empty())
                        .collect();
                }
                _ => {}
            }
        }
    } else if let Some(id) = group.strip_prefix("Desktop Action ") {
        let mut action = Action { id: id.to_owned(), ..Action::default() };
        for (&key, &(_, value)) in values.iter() {
            match key {
                "Name" => value.clone_into(&mut action.name),
                "Icon" => value.clone_into(&mut action.icon),
                "Exec" => {
                    value.clone_into(&mut action.exec);
                    action.command = parse_exec(value);
                }
                _ => {}
            }
        }
        pending.insert(id, action);
    }
    values.clear();
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(';')
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Splits `Exec` into an argv. Inside quotes, field codes stay as written and four
/// backslashes give one.
pub fn parse_exec(exec: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escape = 0u8;
    let mut percent = false;
    for c in exec.chars() {
        if escape == 0 && c == '\\' {
            escape = 1;
        } else if quoted {
            if c == '\\' {
                escape += 1;
                if escape == 4 {
                    current.push('\\');
                    escape = 0;
                }
            } else if escape == 2 {
                current.push(c);
                escape = 0;
            } else if escape != 0 {
                match c {
                    's' => current.push(' '),
                    'n' => current.push('\n'),
                    't' => current.push('\t'),
                    'r' => current.push('\r'),
                    _ => {
                        debug!(exec, "illegal escape in an Exec value");
                        current.push(c);
                    }
                }
                escape = 0;
            } else if c == '"' || c == '\'' {
                quoted = false;
            } else {
                current.push(c);
            }
        } else if escape != 0 {
            current.push(c);
            escape = 0;
        } else if percent {
            if c == '%' {
                current.push('%');
            }
            percent = false;
        } else if c == '%' {
            percent = true;
        } else if c == '"' || c == '\'' {
            quoted = true;
        } else if c == ' ' {
            if !current.is_empty() {
                arguments.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(text: &str) -> Vec<String> {
        parse_exec(text)
    }

    #[test]
    fn exec_quoting_and_field_codes() {
        assert_eq!(exec("firefox %u"), ["firefox"]);
        assert_eq!(exec("  a   b  "), ["a", "b"]);
        assert_eq!(exec("app --x=%f %F %i %c %k"), ["app", "--x="]);
        assert_eq!(exec("echo 100%%"), ["echo", "100%"]);
        assert_eq!(
            exec(r#"sh -c "echo 'hi there'""#),
            ["sh", "-c", "echo hi", "there"]
        );
        assert_eq!(exec(r#""a b" 'c d'"#), ["a b", "c d"]);
        assert_eq!(exec(r#""%f" x"#), ["%f", "x"]);
        assert_eq!(exec(r#"a\ b"#), ["a b"]);
        assert_eq!(exec(r#""a\sb\tc""#), ["a b\tc"]);
        assert_eq!(exec(r#""say \\"hi\\"""#), [r#"say "hi""#]);
        assert_eq!(exec(r#""back\\\\slash""#), [r"back\slash"]);
        assert_eq!(exec(r#"x "" y"#), ["x", "y"]);
        assert_eq!(exec("ไทย --flag"), ["ไทย", "--flag"]);
        assert!(exec("").is_empty());
    }

    #[test]
    fn localized_keys_follow_the_system() {
        let text = "[Desktop Entry]\nType=Application\nName[de]=Rechner\nName=Calculator\n\
                    Name[th]=เครื่องคิดเลข\nComment[th_TH]=คิดเลข\nComment=Compute\n";
        let thai = Locale::parse("th_TH.UTF-8");
        let parsed = parse("calc", text, &thai);
        assert_eq!(parsed.entry.name, "เครื่องคิดเลข");
        assert_eq!(parsed.entry.comment, "คิดเลข");
        let english = Locale::parse("en_US.UTF-8");
        let parsed = parse("calc", text, &english);
        assert_eq!(
            parsed.entry.name, "Calculator",
            "an unlocalized key replaces a non-matching one"
        );
        assert_eq!(parsed.entry.comment, "Compute");
        let only_localized = "[Desktop Entry]\nType=Application\nName[de]=Rechner\n";
        assert_eq!(
            parse("x", only_localized, &english)
                .entry
                .name,
            "Rechner",
            "the first value stays when nothing scores higher"
        );
    }

    #[test]
    fn groups_types_and_flags() {
        let text = "# comment\n[Desktop Entry]\nType=Application\nName=Foot\nExec=foot\n\
                    Terminal=true\nNoDisplay=false\nHidden=TRUE\nCategories=System;;Terminal;\n\
                    Keywords=shell;prompt\nPath=/srv/work\nStartupWMClass=foot\nIcon=foot\n\
                    Actions=server;missing;new;\n\n[Desktop Action new]\nName=New window\nExec=foot --new\n\
                    [Desktop Action server]\nName=Server\nExec=foot --server\nIcon=foot-server\n";
        let parsed = parse("org.codeberg.dnkl.foot", text, &Locale::default());
        let entry = &parsed.entry;
        assert!(!parsed.hidden, "only the exact value true counts");
        assert!(entry.terminal);
        assert!(!entry.no_display);
        assert_eq!(entry.categories, ["System", "Terminal"]);
        assert_eq!(entry.keywords, ["shell", "prompt"]);
        assert_eq!(entry.working_directory, "/srv/work");
        assert_eq!(entry.startup_class, "foot");
        let actions: Vec<(&str, &str, &[String])> = entry
            .actions
            .iter()
            .map(|action| {
                (
                    action.id.as_str(),
                    action.name.as_str(),
                    action.command.as_slice(),
                )
            })
            .collect();
        assert_eq!(
            actions,
            [
                (
                    "server",
                    "Server",
                    [String::from("foot"), String::from("--server")].as_slice()
                ),
                (
                    "new",
                    "New window",
                    [String::from("foot"), String::from("--new")].as_slice()
                ),
            ]
        );
        assert_eq!(entry.actions[0].icon, "foot-server");

        let link = "[Desktop Entry]\nType=Link\nName=Site\nURL=https://example.org\n";
        assert!(
            !parse("site", link, &Locale::default())
                .entry
                .is_valid()
        );
        let untyped = "[Desktop Entry]\nName=No type\n";
        assert!(
            !parse("x", untyped, &Locale::default())
                .entry
                .is_valid()
        );
        let spaced = "[Desktop Entry]\nType=Application\nName = Spaced\n";
        assert!(
            !parse("x", spaced, &Locale::default())
                .entry
                .is_valid(),
            "keys are not trimmed"
        );
    }
}
