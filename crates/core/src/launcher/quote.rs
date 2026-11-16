//! Quoting for floating launches through a Lua dispatch: Lua long strings and POSIX shell words.

pub const FLOATING_SIZE: &str = "monitor_h*1.1 monitor_h*0.75";

/// A Lua long bracket string holding `text` verbatim. The level is the lowest whose closing
/// bracket `text` cannot end early, so untrusted text cannot break out of the string.
pub fn lua_string(text: &str) -> String {
    let closed = format!("{text}]");
    let mut closing = String::from("]]");
    while closed.contains(&closing) {
        closing.insert(1, '=');
    }
    let level = &closing[1..closing.len() - 1];
    format!("[{level}[{text}]{level}]")
}

pub fn shell_quote(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    push_quoted(&mut quoted, arg);
    quoted
}

pub fn shell_command<S: AsRef<str>>(command: &[S], working_directory: &str) -> String {
    let mut line = String::new();
    if !working_directory.is_empty() {
        line.push_str("cd ");
        push_quoted(&mut line, working_directory);
        line.push_str(" && ");
    }
    for (index, arg) in command.iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        push_quoted(&mut line, arg.as_ref());
    }
    line
}

pub fn float_dispatch<S: AsRef<str>>(shell_command: &str, tenant_classes: &[S]) -> String {
    let command = lua_string(shell_command);
    let size = lua_string(FLOATING_SIZE);
    if tenant_classes.is_empty() {
        return format!(
            "hl.dsp.exec_cmd({command}, {{ float = true, center = true, size = {size} }})"
        );
    }
    let classes: Vec<String> = tenant_classes
        .iter()
        .map(|class| lua_string(class.as_ref()))
        .collect();
    format!(
        "tenant_float_launch({command}, {{ {} }}, {size})",
        classes.join(", ")
    )
}

fn push_quoted(line: &mut String, arg: &str) {
    line.push('\'');
    for c in arg.chars() {
        if c == '\'' {
            line.push_str("'\\''");
        } else {
            line.push(c);
        }
    }
    line.push('\'');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_levels() {
        assert_eq!(lua_string(""), "[[]]");
        assert_eq!(lua_string("a b"), "[[a b]]");
        assert_eq!(lua_string("x]]y"), "[=[x]]y]=]");
        assert_eq!(lua_string("x]"), "[=[x]]=]");
        assert_eq!(lua_string("x]="), "[[x]=]]");
        assert_eq!(lua_string("]]]=]"), "[==[]]]=]]==]");
        assert_eq!(lua_string("[[ok"), "[[[[ok]]");
    }

    #[test]
    fn shell_words() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(
            shell_command(&["uwsm-app", "--", "a b"], ""),
            "'uwsm-app' '--' 'a b'"
        );
        assert_eq!(
            shell_command(&["x"], "/tmp/it's"),
            "cd '/tmp/it'\\''s' && 'x'"
        );
        assert_eq!(shell_command::<&str>(&[], ""), "");
    }

    #[test]
    fn dispatches() {
        assert_eq!(
            float_dispatch::<&str>("'kitty'", &[]),
            "hl.dsp.exec_cmd([['kitty']], { float = true, center = true, size = [[monitor_h*1.1 monitor_h*0.75]] })"
        );
        assert_eq!(
            float_dispatch("'game'", &["Game", "game.desktop"]),
            "tenant_float_launch([['game']], { [[Game]], [[game.desktop]] }, [[monitor_h*1.1 monitor_h*0.75]])"
        );
    }
}
