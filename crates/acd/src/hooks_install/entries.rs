//! The hook entries acd owns (spec §6.2, §6.3 tables).
use serde_json::{Value, json};

use super::{Ctx, MARKER};

/// Claude: `(event, matcher, timeout seconds)`. Claude never sends
/// `SessionStart` over its HTTP hook.
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

pub fn claude_group(
    ctx: &Ctx,
    token: &str,
    event: &str,
    matcher: Option<&str>,
    timeout: u64,
) -> Value {
    let entry = json!({
        "type": "http",
        "url": format!("http://127.0.0.1:{}/hook/claude/{event}?marker={MARKER}", ctx.hook_port),
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
    let command = format!(
        "{} hook codex {event} --config-dir {} {MARKER}",
        sh_quote(&ctx.exe.to_string_lossy()),
        sh_quote(&ctx.config_dir.to_string_lossy())
    );
    json!({"hooks": [{"type": "command", "command": command, "timeout": timeout}]})
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

fn claude_owned(event: &str, entry: &Value) -> bool {
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

fn codex_owned(event: &str, entry: &Value) -> bool {
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
        [exe, hook, codex, found_event, flag, dir, marker]
            if exe.starts_with('/') && hook == "hook" && codex == "codex" && found_event == event
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

    #[test]
    fn owned_entries_carry_the_marker_and_environment_allowlist() {
        let context = ctx();
        let claude = claude_group(&context, "tok", "Stop", None, 1);
        assert_eq!(classify("Stop", &claude["hooks"][0]), Ownership::Owned);
        assert_eq!(
            claude["hooks"][0]["url"],
            "http://127.0.0.1:47801/hook/claude/Stop?marker=acd-managed:v1"
        );
        assert_eq!(
            claude["hooks"][0]["headers"]["X-Herdr-Pane"],
            "$HERDR_PANE_ID"
        );
        assert_eq!(claude["hooks"][0]["allowedEnvVars"][0], "HERDR_PANE_ID");
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
    fn ownership_is_exact() {
        let context = ctx();
        let owned_claude = claude_group(&context, "tok", "Stop", None, 1)["hooks"][0].clone();
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
