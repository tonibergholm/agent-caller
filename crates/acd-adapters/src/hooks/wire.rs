//! Hook payloads, names and replies (spec §6.1 to §6.3). Payloads are read
//! field by field: a field of the wrong type is absent, never fatal, so a
//! future Claude or Codex version fails open instead of breaking acd.
use acd_core::CorrelationKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const MAX_ID_BYTES: usize = 128;
pub const MAX_HINT_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HookAgent {
    Claude,
    Codex,
}

impl HookAgent {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookName {
    SessionStart,
    UserPromptSubmit,
    PermissionRequest,
    PreToolUse,
    PostToolUse,
    Stop,
    SessionEnd,
    /// Notification and anything newer: logged and ignored.
    Other,
}

impl HookName {
    pub fn parse(s: &str) -> Self {
        match s {
            "SessionStart" => Self::SessionStart,
            "UserPromptSubmit" => Self::UserPromptSubmit,
            "PermissionRequest" => Self::PermissionRequest,
            "PreToolUse" => Self::PreToolUse,
            "PostToolUse" => Self::PostToolUse,
            "Stop" => Self::Stop,
            "SessionEnd" => Self::SessionEnd,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Payload {
    pub session_id: String,
    pub prompt_id: Option<String>,
    pub turn_id: Option<String>,
    pub cwd: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Value,
    pub tool_response: Value,
    pub permission_suggestions: Vec<Value>,
    pub last_assistant_message: Option<String>,
    /// Present on subagent payloads, which are always dropped (spec §6.2).
    pub agent_id: Option<String>,
}

pub fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID_BYTES && !s.chars().any(char::is_control)
}

impl Payload {
    pub fn from_value(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_owned);
        let id = |k: &str| s(k).filter(|x| valid_id(x));
        Some(Self {
            session_id: id("session_id")?,
            prompt_id: id("prompt_id"),
            turn_id: id("turn_id"),
            cwd: s("cwd"),
            tool_name: s("tool_name"),
            tool_input: o.get("tool_input").cloned().unwrap_or(Value::Null),
            tool_response: o.get("tool_response").cloned().unwrap_or(Value::Null),
            permission_suggestions: o
                .get("permission_suggestions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            last_assistant_message: s("last_assistant_message"),
            agent_id: s("agent_id"),
        })
    }

    pub fn is_subagent(&self) -> bool {
        self.agent_id.as_deref().is_some_and(|a| !a.is_empty())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    pub herdr_pane: Option<String>,
    pub herdr_socket: Option<String>,
    pub entrypoint: Option<String>,
}

pub fn hint(v: Option<&str>) -> Option<String> {
    v.filter(|s| !s.is_empty() && s.len() <= MAX_HINT_BYTES && !s.chars().any(char::is_control))
        .map(str::to_owned)
}

impl Hints {
    pub fn from_headers(h: &axum::http::HeaderMap) -> Self {
        let get = |name: &str| hint(h.get(name).and_then(|v| v.to_str().ok()));
        Self {
            herdr_pane: get("x-herdr-pane"),
            herdr_socket: get("x-herdr-socket"),
            entrypoint: get("x-claude-entrypoint"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HookEvent {
    pub agent: HookAgent,
    pub name: HookName,
    pub payload: Payload,
    pub hints: Hints,
    pub at: tokio::time::Instant,
}

pub fn canonical_json(v: &Value) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String((*k).clone()).to_string());
                    out.push(':');
                    write(&m[*k], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut s = String::new();
    write(v, &mut s);
    s
}

pub fn tool_digest(tool_name: &str, input: &Value) -> String {
    let mut h = Sha256::new();
    h.update(tool_name.as_bytes());
    h.update([0u8]);
    h.update(canonical_json(input).as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn command_of(input: &Value) -> Option<&str> {
    input.get("command").and_then(Value::as_str)
}

pub fn claude_key(p: &Payload) -> Option<CorrelationKey> {
    Some(CorrelationKey(format!(
        "claude:{}:{}:{}",
        p.session_id,
        p.prompt_id.as_deref()?,
        tool_digest(p.tool_name.as_deref()?, &p.tool_input)
    )))
}

pub fn codex_key(p: &Payload) -> Option<CorrelationKey> {
    Some(CorrelationKey(format!(
        "codex:{}:{}:{}",
        p.session_id,
        p.turn_id.as_deref()?,
        tool_digest(p.tool_name.as_deref()?, &p.tool_input)
    )))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlwaysRule {
    pub tool_name: String,
    pub rule_content: String,
}

/// The single rule construction used for display and delivery (ruling Q2):
/// a Bash request's exact command, else the first `addRules` suggestion.
/// Claude reads `*` in a Bash rule as a wildcard and `(`, `)` delimit the
/// rule, so a command holding any of them has no safe exact rule: no
/// option is offered rather than a broader one than the user saw.
pub fn always_rule(p: &Payload) -> Option<AlwaysRule> {
    if p.tool_name.as_deref() == Some("Bash")
        && let Some(c) = command_of(&p.tool_input)
    {
        if c.contains(['*', '(', ')']) {
            return None;
        }
        return Some(AlwaysRule {
            tool_name: "Bash".into(),
            rule_content: c.into(),
        });
    }
    p.permission_suggestions.iter().find_map(|s| {
        if s.get("type")?.as_str()? != "addRules" {
            return None;
        }
        let r = s.get("rules")?.as_array()?.first()?;
        Some(AlwaysRule {
            tool_name: r.get("toolName")?.as_str()?.into(),
            rule_content: r.get("ruleContent")?.as_str()?.into(),
        })
    })
}

fn envelope(decision: Value) -> String {
    json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}})
        .to_string()
}
pub fn reply_allow() -> String {
    envelope(json!({"behavior": "allow"}))
}
pub fn reply_deny() -> String {
    envelope(json!({"behavior": "deny", "message": "Denied from agent inbox"}))
}
pub fn reply_allow_always(r: &AlwaysRule) -> String {
    envelope(
        json!({"behavior":"allow", "updatedPermissions":[{"type":"addRules","rules":[{"toolName":r.tool_name,"ruleContent":r.rule_content}],"behavior":"allow","destination":"localSettings"}]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fx(rel: &str) -> Value {
        let p = format!(
            "{}/../../tests/fixtures/hooks/{rel}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }
    fn payload(rel: &str) -> Payload {
        Payload::from_value(&fx(rel)).unwrap()
    }
    #[test]
    fn every_fixture_payload_parses() {
        for rel in [
            "claude/payload-PermissionRequest-Bash.json",
            "claude/payload-PermissionRequest-AskUserQuestion.json",
            "claude/payload-PreToolUse-AskUserQuestion.json",
            "claude/payload-PostToolUse-Bash.json",
            "claude/payload-PostToolUse-AskUserQuestion.json",
            "claude/payload-Stop.json",
            "claude/payload-Stop-after-deny.json",
            "claude/payload-UserPromptSubmit.json",
            "claude/payload-SessionStart.json",
            "claude/payload-Notification.json",
            "codex/payload-PermissionRequest-Bash.json",
            "codex/payload-PostToolUse-Bash.json",
            "codex/payload-SessionStart.json",
            "codex/payload-SessionStart-embedded-correct-env.json",
            "codex/payload-SessionStart-fresh-app-server-other-pane-env.json",
            "codex/payload-SessionStart-shared-daemon-stale-env.json",
            "codex/payload-UserPromptSubmit.json",
        ] {
            let p = payload(rel);
            assert!(valid_id(&p.session_id), "{rel}");
        }
    }
    #[test]
    fn missing_or_hostile_ids_are_rejected() {
        assert!(Payload::from_value(&json!({"cwd":"/x"})).is_none());
        assert!(Payload::from_value(&json!([1, 2])).is_none());
        assert!(Payload::from_value(&json!({"session_id":"a".repeat(129)})).is_none());
        assert!(Payload::from_value(&json!({"session_id":"a\u{7}b"})).is_none());
        let p = Payload::from_value(
            &json!({"session_id":"s","prompt_id":5,"permission_suggestions":{"x":1}}),
        )
        .unwrap();
        assert_eq!((p.prompt_id, p.permission_suggestions.len()), (None, 0));
    }
    #[test]
    fn same_turn_different_tools_get_different_keys() {
        let a = payload("claude/payload-PermissionRequest-Bash.json");
        let mut b = a.clone();
        b.tool_input["command"] = "touch /tmp/acp0/other.txt".into();
        assert_eq!(a.prompt_id, b.prompt_id);
        assert_ne!(claude_key(&a), claude_key(&b));
        let mut c = a.clone();
        c.tool_input = serde_json::from_str(
            r#"{"description":"Create empty file","command":"touch /tmp/acp0/t1.txt"}"#,
        )
        .unwrap();
        assert_eq!(claude_key(&a), claude_key(&c));
    }
    #[test]
    fn question_pre_tool_use_and_permission_request_share_a_key() {
        let a = payload("claude/payload-PreToolUse-AskUserQuestion.json");
        let b = payload("claude/payload-PermissionRequest-AskUserQuestion.json");
        assert_eq!(claude_key(&a), claude_key(&b));
        assert!(claude_key(&a).unwrap().0.starts_with("claude:4e3885ee-"));
    }
    #[test]
    fn codex_post_tool_use_matches_by_command_not_digest() {
        let a = payload("codex/payload-PermissionRequest-Bash.json");
        let b = payload("codex/payload-PostToolUse-Bash.json");
        assert_ne!(
            tool_digest("Bash", &a.tool_input),
            tool_digest("Bash", &b.tool_input)
        );
        assert_eq!(command_of(&a.tool_input), command_of(&b.tool_input));
        assert!(codex_key(&a).unwrap().0.starts_with("codex:01a116d3-"));
        assert!(codex_key(&b).is_some());
    }
    #[test]
    fn replies_match_the_recorded_fixtures() {
        let v = |s: String| serde_json::from_str::<Value>(&s).unwrap();
        assert_eq!(v(reply_allow()), fx("claude/response-allow.json"));
        assert_eq!(v(reply_deny()), fx("claude/response-deny.json"));
        let r = AlwaysRule {
            tool_name: "Bash".into(),
            rule_content: "touch /tmp/acp0/*".into(),
        };
        assert_eq!(
            v(reply_allow_always(&r)),
            fx("claude/response-allow-always-localSettings.json")
        );
    }
    #[test]
    fn bash_rules_are_the_exact_command_and_others_need_a_suggestion() {
        let p = payload("claude/payload-PermissionRequest-Bash.json");
        assert_eq!(
            always_rule(&p),
            Some(AlwaysRule {
                tool_name: "Bash".into(),
                rule_content: "touch /tmp/acp0/t1.txt".into()
            })
        );
        let mut e = p.clone();
        e.tool_name = Some("Edit".into());
        e.tool_input = json!({"file_path":"/x"});
        assert_eq!(always_rule(&e), None);
        e.permission_suggestions =
            vec![json!({"type":"addRules","rules":[{"toolName":"Edit","ruleContent":"/x"}]})];
        assert_eq!(
            always_rule(&e),
            Some(AlwaysRule {
                tool_name: "Edit".into(),
                rule_content: "/x".into()
            })
        );
    }
    #[test]
    fn bash_commands_with_wildcard_or_paren_characters_get_no_always_rule() {
        let mut p = payload("claude/payload-PermissionRequest-Bash.json");
        for cmd in ["ls *", "foo :*", "echo (a)", "echo a)"] {
            p.tool_input["command"] = cmd.into();
            assert_eq!(always_rule(&p), None, "{cmd}");
        }
        p.tool_input["command"] = "ls /tmp".into();
        assert_eq!(always_rule(&p).unwrap().rule_content, "ls /tmp");
    }
    #[test]
    fn subagent_payloads_are_recognised() {
        let mut p = payload("claude/payload-Stop.json");
        assert!(!p.is_subagent());
        p.agent_id = Some(String::new());
        assert!(!p.is_subagent());
        p.agent_id = Some("agent-1".into());
        assert!(p.is_subagent());
        let v = json!({"session_id":"s","agent_id":"a1"});
        assert!(Payload::from_value(&v).unwrap().is_subagent());
        assert!(
            !Payload::from_value(&json!({"session_id":"s"}))
                .unwrap()
                .is_subagent()
        );
    }
    #[test]
    fn hints_are_absent_when_empty_long_or_controlled() {
        assert_eq!(hint(Some("")), None);
        assert_eq!(hint(Some(&"x".repeat(1025))), None);
        assert_eq!(hint(Some("w1:p2\t")), None);
        assert_eq!(hint(Some("w1:p2")).as_deref(), Some("w1:p2"));
    }
    #[test]
    fn names_parse_and_unknown_is_other() {
        assert_eq!(
            HookName::parse("PermissionRequest"),
            HookName::PermissionRequest
        );
        assert_eq!(HookName::parse("Notification"), HookName::Other);
        assert_eq!(HookAgent::parse("codex"), Some(HookAgent::Codex));
        assert_eq!(HookAgent::parse("aider"), None);
    }
}
