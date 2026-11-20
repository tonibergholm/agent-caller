//! The hook entries acd owns (spec §6.2, §6.3 tables).
use serde_json::{Value, json};

use super::{Ctx, MARKER};

/// Claude: `(event, matcher, timeout seconds)`. Claude never sends
/// `SessionStart` to acd.
pub fn claude_events(permission_hold: u64) -> Vec<(&'static str, Option<&'static str>, u64)> {
    vec![
        ("UserPromptSubmit", None, 1),
        ("PermissionRequest", None, permission_hold + 10),
        ("PreToolUse", Some("AskUserQuestion"), 1),
        ("PostToolUse", None, 1),
        ("PostToolUseFailure", None, 1),
        ("Stop", None, 1),
        ("SessionEnd", None, 1),
    ]
}

pub const CODEX_EVENTS: [(&str, u64); 5] = [
    ("SessionStart", 2),
    ("UserPromptSubmit", 2),
    ("PermissionRequest", 1),
    ("PostToolUse", 2),
    ("Stop", 2),
];

/// The command hook for `agent` (`acd hook <agent> <event>`, the shim over
/// `hook.sock`). It carries no token: the shim reads it from the config dir.
fn command_entry(ctx: &Ctx, agent: &str, event: &str, timeout: u64) -> Value {
    let command = format!(
        "{} hook {agent} {event} --config-dir {} {MARKER}",
        sh_quote(&ctx.exe.to_string_lossy()),
        sh_quote(&ctx.config_dir.to_string_lossy())
    );
    json!({"type": "command", "command": command, "timeout": timeout})
}

pub fn claude_group(ctx: &Ctx, event: &str, matcher: Option<&str>, timeout: u64) -> Value {
    let entry = command_entry(ctx, "claude", event, timeout);
    match matcher {
        Some(value) => json!({"matcher": value, "hooks": [entry]}),
        None => json!({"hooks": [entry]}),
    }
}

/// The group earlier versions wrote for Claude (an http hook with the token in
/// its headers), for tests of the migration and of the token rewrite.
#[cfg(test)]
pub fn legacy_claude_group(
    port: u16,
    token: &str,
    event: &str,
    matcher: Option<&str>,
    timeout: u64,
) -> Value {
    let entry = json!({
        "type": "http",
        "url": format!("http://127.0.0.1:{port}/hook/claude/{event}?marker={MARKER}"),
        "timeout": timeout,
        "headers": {
            "X-Acd-Token": token,
            "X-Herdr-Pane": "$HERDR_PANE_ID",
            "X-Herdr-Socket": "$HERDR_SOCKET_PATH",
            "X-Claude-Entrypoint": "$CLAUDE_CODE_ENTRYPOINT"
        },
        "allowedEnvVars": ["HERDR_PANE_ID", "HERDR_SOCKET_PATH", "CLAUDE_CODE_ENTRYPOINT"]
    });
    match matcher {
        Some(value) => json!({"matcher": value, "hooks": [entry]}),
        None => json!({"hooks": [entry]}),
    }
}

/// POSIX single-quote quoting.
pub fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn codex_group(ctx: &Ctx, event: &str, timeout: u64) -> Value {
    json!({"hooks": [command_entry(ctx, "codex", event, timeout)]})
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ownership {
    Owned,
    Suspicious,
    Foreign,
}

/// Splits bare words and POSIX single-quoted words; shell syntax is rejected.
pub fn sh_words(cmd: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => cur.push(ch),
                    }
                }
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    if chars.next()? != '\'' {
                        return None;
                    }
                    cur.push('\'');
                    if chars.next()? != '\'' {
                        return None;
                    }
                    loop {
                        match chars.next()? {
                            '\'' => break,
                            ch => cur.push(ch),
                        }
                    }
                }
            }
            c if c.is_ascii_alphanumeric() || "-_./:".contains(c) => {
                in_word = true;
                cur.push(c);
            }
            _ => return None,
        }
    }
    if in_word {
        words.push(cur);
    }
    Some(words)
}

/// The http hook earlier versions wrote for Claude: it carries the hook token
/// in its headers. Still owned, so a reinstall replaces it and an uninstall
/// removes it; `acd hooks status` warns about it.
pub fn claude_http_owned(event: &str, entry: &Value) -> bool {
    if entry.get("type").and_then(Value::as_str) != Some("http") {
        return false;
    }
    let Some(url) = entry.get("url").and_then(Value::as_str) else {
        return false;
    };
    let Some(rest) = url.strip_prefix("http://127.0.0.1:") else {
        return false;
    };
    let Some((port, path)) = rest.split_once('/') else {
        return false;
    };
    port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p > 0)
        && !port.starts_with('0')
        && path == format!("hook/claude/{event}?marker={MARKER}")
}

fn claude_owned(event: &str, entry: &Value) -> bool {
    claude_http_owned(event, entry) || command_owned("claude", event, entry)
}

fn codex_owned(event: &str, entry: &Value) -> bool {
    command_owned("codex", event, entry)
}

fn command_owned(agent: &str, event: &str, entry: &Value) -> bool {
    if entry.get("type").and_then(Value::as_str) != Some("command") {
        return false;
    }
    let Some(words) = entry
        .get("command")
        .and_then(Value::as_str)
        .and_then(sh_words)
    else {
        return false;
    };
    matches!(words.as_slice(),
        [exe, hook, found_agent, found_event, flag, dir, marker]
            if exe.starts_with('/') && hook == "hook" && found_agent == agent && found_event == event
               && flag == "--config-dir" && dir.starts_with('/') && marker == MARKER)
}

/// Like `classify`, but only the given target's own shape is `Owned`.
pub fn classify_for(target: super::Target, event: &str, entry: &Value) -> Ownership {
    let owned = match target {
        super::Target::Claude => claude_owned(event, entry),
        super::Target::Codex => codex_owned(event, entry),
    };
    if owned {
        Ownership::Owned
    } else if entry.to_string().contains(MARKER) {
        Ownership::Suspicious
    } else {
        Ownership::Foreign
    }
}

pub fn classify(event: &str, entry: &Value) -> Ownership {
    if claude_owned(event, entry) || codex_owned(event, entry) {
        Ownership::Owned
    } else if entry.to_string().contains(MARKER) {
        Ownership::Suspicious
    } else {
        Ownership::Foreign
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks_install::Target;

    fn ctx() -> Ctx {
        Ctx {
            home: "/h".into(),
            claude_dir: "/h/.claude".into(),
            codex_dir: "/h/.codex".into(),
            config_dir: "/h/.config/acd".into(),
            exe: "/opt/my tools/acd".into(),
            hook_port: 47801,
            permission_hold: 3600,
        }
    }

    #[test]
    fn quoting_survives_spaces_and_quotes() {
        assert_eq!(sh_quote("/opt/my tools/a'b"), "'/opt/my tools/a'\\''b'");
    }

    fn legacy_http(event: &str) -> Value {
        legacy_claude_group(47801, "tok", event, None, 1)["hooks"][0].clone()
    }

    #[test]
    fn owned_entries_run_the_shim_and_carry_the_marker_and_no_token() {
        let context = ctx();
        let claude = claude_group(&context, "Stop", None, 1);
        assert_eq!(classify("Stop", &claude["hooks"][0]), Ownership::Owned);
        assert_eq!(
            claude["hooks"][0],
            json!({
                "type": "command",
                "command": "'/opt/my tools/acd' hook claude Stop --config-dir '/h/.config/acd' acd-managed:v1",
                "timeout": 1
            })
        );
        let text = claude.to_string();
        assert!(!text.contains("http") && !text.contains("X-Acd-Token"), "{text}");
        let codex = codex_group(&context, "Stop", 2);
        assert_eq!(
            codex["hooks"][0]["command"],
            "'/opt/my tools/acd' hook codex Stop --config-dir '/h/.config/acd' acd-managed:v1"
        );
        assert_eq!(
            classify(
                "Stop",
                &serde_json::json!({"type": "command", "command": "say done"})
            ),
            Ownership::Foreign
        );
        assert_eq!(claude_events(3600)[1], ("PermissionRequest", None, 3610));
        assert!(
            claude_events(3600).contains(&("PostToolUseFailure", None, 1)),
            "failed tools count as tool-ran evidence"
        );
        assert!(
            !claude_events(3600)
                .iter()
                .any(|(event, _, _)| *event == "SessionStart")
        );
    }

    #[test]
    fn the_legacy_http_entry_is_still_owned_but_only_for_claude() {
        let http = legacy_http("Stop");
        assert_eq!(classify("Stop", &http), Ownership::Owned);
        assert_eq!(
            classify_for(Target::Claude, "Stop", &http),
            Ownership::Owned
        );
        assert!(claude_http_owned("Stop", &http));
        assert!(!claude_http_owned(
            "Stop",
            &claude_group(&ctx(), "Stop", None, 1)["hooks"][0]
        ));
        assert_eq!(
            classify_for(Target::Codex, "Stop", &http),
            Ownership::Suspicious
        );
    }

    #[test]
    fn ownership_is_exact() {
        let context = ctx();
        let owned_claude = claude_group(&context, "Stop", None, 1)["hooks"][0].clone();
        let owned_codex = codex_group(&context, "Stop", 2)["hooks"][0].clone();
        assert_eq!(classify("Stop", &owned_claude), Ownership::Owned);
        assert_eq!(classify("Stop", &owned_codex), Ownership::Owned);
        assert_eq!(
            classify("PostToolUse", &owned_claude),
            Ownership::Suspicious,
            "wrong event"
        );
        for entry in [
            json!({"type": "command", "command": "echo acd-managed:v1 >> log"}),
            json!({"type": "command", "command": "'/bin/acd' hook claude Stop --config-dir '/h' acd-managed:v1 extra"}),
            json!({"type": "command", "command": "'bin/acd' hook claude Stop --config-dir '/h' acd-managed:v1"}),
            json!({"type": "http", "url": "https://example.com/x?note=acd-managed:v1"}),
            json!({"type": "http", "url": "http://10.0.0.1:47801/hook/claude/Stop?marker=acd-managed:v1"}),
            json!({"type": "http", "url": "http://127.0.0.1:47801/hook/claude/Stop?marker=acd-managed:v1&x=1"}),
            json!({"type": "http", "url": "http://127.0.0.1:+47801/hook/claude/Stop?marker=acd-managed:v1"}),
        ] {
            assert_eq!(classify("Stop", &entry), Ownership::Suspicious, "{entry}");
        }
        assert_eq!(
            classify("Stop", &json!({"type": "command", "command": "say done"})),
            Ownership::Foreign
        );
    }

    #[test]
    fn sh_words_round_trips_the_quoting() {
        assert_eq!(
            sh_words(&format!("{} hook", sh_quote("/opt/my tools/a'b"))).unwrap(),
            vec!["/opt/my tools/a'b", "hook"]
        );
        let two = "/opt/a''b";
        assert_eq!(sh_quote(two), "'/opt/a'\\'''\\''b'");
        assert_eq!(sh_words(&sh_quote(two)).unwrap(), vec![two]);
        let three = "'''";
        assert_eq!(sh_words(&sh_quote(three)).unwrap(), vec![three]);
        for bad in ["a;b", "a $(x)", "a \"b\"", "a `b`", "a\\b", "'open"] {
            assert!(sh_words(bad).is_none(), "{bad}");
        }
    }
}
