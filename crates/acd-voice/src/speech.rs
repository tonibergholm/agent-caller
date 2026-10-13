//! Templated spoken text (spec §7). No model text: every field is cleaned
//! and cut before it is spoken.
use acd_protocol::{AgentKind, RequestKind};

use crate::snapshot::{CardSnap, Group, RequestSnap, Snapshot};
use crate::text::{VoiceText, clean};

pub const SPOKEN_TITLE_MAX: usize = 60;
pub const READ_MAX: usize = 400;

pub fn kind_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "Claude",
        AgentKind::Codex => "Codex",
        AgentKind::Other | AgentKind::Unknown => "agent",
    }
}

/// A tool name as panel.ts `toolName` accepts it: `^[A-Za-z][A-Za-z0-9_.:-]{0,39}$`.
fn tool_name(title: &str) -> Option<&str> {
    let mut chars = title.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && title.len() <= 40
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c)))
    .then_some(title)
}

/// The panel's type tag, spoken: "bash approval", "question", ...
pub fn request_label(req: &RequestSnap) -> String {
    match req.kind {
        RequestKind::Permission => match tool_name(&req.title) {
            Some(tool) => format!("{} approval", tool.to_lowercase()),
            None => "approval".into(),
        },
        RequestKind::Question => "question".into(),
        RequestKind::BlockedUnparsed => "blocked".into(),
        RequestKind::Unknown => "request".into(),
    }
}

/// Counts and titles of what needs the user, per machine (spec §7).
pub fn summarize(snapshot: &Snapshot, machine: Option<&str>) -> VoiceText {
    let cards: Vec<&CardSnap> = snapshot
        .targetable()
        .into_iter()
        .filter(|c| c.group == Group::Request && machine.is_none_or(|m| c.machine_key == m))
        .collect();
    if cards.is_empty() {
        let place = machine
            .and_then(|m| snapshot.cards.iter().find(|c| c.machine_key == m))
            .map(|c| clean(&c.machine_name, SPOKEN_TITLE_MAX));
        return VoiceText::new(match place {
            Some(p) => format!("Nothing on {p} needs you."),
            None => "Nothing needs you.".into(),
        });
    }
    let verb = if cards.len() == 1 { "needs" } else { "need" };
    let mut out = format!("{} {verb} you.", cards.len());
    let mut machines: Vec<&str> = Vec::new();
    for c in &cards {
        if !machines.contains(&c.machine_key.as_str()) {
            machines.push(&c.machine_key);
        }
    }
    for m in machines {
        let here: Vec<&&CardSnap> = cards.iter().filter(|c| c.machine_key == m).collect();
        let items: Vec<String> = here
            .iter()
            .map(|c| {
                let what = c.pending.as_ref().map(request_label).unwrap_or_else(|| "needs you".into());
                format!("{}, {}: {what}", kind_name(c.kind), clean(&c.title, SPOKEN_TITLE_MAX))
            })
            .collect();
        out.push_str(&format!(" On {}: {}.", clean(&here[0].machine_name, SPOKEN_TITLE_MAX), items.join("; ")));
    }
    VoiceText::new(out)
}

/// The request's question, or its command for a permission (spec §7).
pub fn read(card: &CardSnap) -> Option<VoiceText> {
    let req = card.pending.as_ref()?;
    let lead = match req.kind {
        RequestKind::Permission => {
            let label = request_label(req);
            let mut chars = label.chars();
            chars.next().map(|c| c.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        }
        RequestKind::Question => "Question".to_owned(),
        RequestKind::BlockedUnparsed | RequestKind::Unknown => clean(&req.title, SPOKEN_TITLE_MAX),
    };
    let body = match req.kind {
        RequestKind::Question => req.question.as_deref().unwrap_or(&req.detail),
        _ => &req.detail,
    };
    Some(VoiceText::new(format!("{lead}. {}", clean(body, READ_MAX))))
}

/// "minos Claude needs you: bash approval" (spec §7). Built, not fired,
/// in step 1.
pub fn alert(card: &CardSnap) -> Option<VoiceText> {
    let req = card.pending.as_ref()?;
    Some(VoiceText::new(format!(
        "{} {} needs you: {}",
        clean(&card.machine_name, SPOKEN_TITLE_MAX),
        kind_name(card.kind),
        request_label(req)
    )))
}

/// One alert for every request since the last one (plan V14): the first
/// named, the rest counted.
pub fn alerts(cards: &[&CardSnap]) -> Option<VoiceText> {
    let first = alert(cards.first()?)?;
    Some(match cards.len() - 1 {
        0 => first,
        1 => VoiceText::new(format!("{}. 1 more needs you.", first.expose())),
        n => VoiceText::new(format!("{}. {n} more need you.", first.expose())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::fixtures::*;
    use crate::snapshot::Group;
    use acd_protocol::{AgentRole, Effect};

    #[test]
    fn several_alerts_name_the_first_and_count_the_rest() {
        let (a, b, c) = (pending("minos", "a"), pending("minos", "b"), pending("studio", "c"));
        assert_eq!(alerts(&[&a]).unwrap().expose(), "minos Claude needs you: bash approval");
        assert_eq!(alerts(&[&a, &b, &c]).unwrap().expose(), "minos Claude needs you: bash approval. 2 more need you.");
        assert!(alerts(&[]).is_none());
    }

    #[test]
    fn labels_follow_the_panel_type_tag() {
        let mut r = permission("r");
        assert_eq!(request_label(&r), "bash approval");
        r.title = "Run npm test?".into();
        assert_eq!(request_label(&r), "approval");
        r.title = "mcp__srv__tool".into();
        assert_eq!(request_label(&r), "mcp__srv__tool approval");
        r.kind = RequestKind::Question;
        assert_eq!(request_label(&r), "question");
        r.kind = RequestKind::BlockedUnparsed;
        assert_eq!(request_label(&r), "blocked");
    }

    #[test]
    fn summaries_count_and_group_by_machine() {
        let mut a = pending("minos", "a");
        a.title = "Fix the build".into();
        let mut b = pending("minos", "b");
        b.kind = AgentKind::Codex;
        b.title = "Docs".into();
        b.pending.as_mut().unwrap().kind = RequestKind::Question;
        let mut c = pending("studio", "c");
        c.title = "Deploy".into();
        let s = snapshot(vec![a, b, c, working("studio", "w")]);
        assert_eq!(
            summarize(&s, None).expose(),
            "3 need you. On minos: Claude, Fix the build: bash approval; Codex, Docs: question. On studio: Claude, Deploy: bash approval."
        );
        assert_eq!(summarize(&s, Some("studio")).expose(), "1 needs you. On studio: Claude, Deploy: bash approval.");
        assert_eq!(summarize(&snapshot(vec![]), None).expose(), "Nothing needs you.");
        let quiet = snapshot(vec![working("minos", "w")]);
        assert_eq!(summarize(&quiet, Some("minos")).expose(), "Nothing on minos needs you.");
    }

    #[test]
    fn summaries_respect_visibility() {
        let mut sub = pending("m", "s");
        sub.role = AgentRole::Subagent;
        let hidden = pending("other", "h");
        let mut s = snapshot(vec![pending("m", "a"), sub, hidden]);
        s.presentation.machine_filter = Some("m".into());
        s.presentation.visible = vec!["m/a".into(), "m/s".into()];
        // The subagent is never spoken; the filter-hidden card is (D16).
        assert!(summarize(&s, None).expose().starts_with("2 need you."));
        assert!(summarize(&s, None).expose().contains("On other:"));
    }

    #[test]
    fn spoken_titles_are_cleaned_and_cut_to_60() {
        let mut a = pending("m", "a");
        a.title = format!("bad\u{202E}title {}", "x".repeat(100));
        let text = summarize(&snapshot(vec![a]), None);
        let title = text.expose().split("Claude, ").nth(1).unwrap().split(": ").next().unwrap();
        assert_eq!(title.chars().count(), SPOKEN_TITLE_MAX);
        assert!(!title.contains('\u{202E}'));
    }

    #[test]
    fn read_speaks_the_command_or_the_question() {
        let a = pending("m", "a");
        assert_eq!(read(&a).unwrap().expose(), "Bash approval. npm test");
        let mut q = pending("m", "q");
        let r = q.pending.as_mut().unwrap();
        r.kind = RequestKind::Question;
        r.question = Some("Which colour?\nPick one".into());
        r.options = vec![option("1", "Red", Effect::Choose)];
        assert_eq!(read(&q).unwrap().expose(), "Question. Which colour? Pick one");
        assert!(read(&working("m", "w")).is_none());
    }

    #[test]
    fn alerts_name_machine_agent_and_request() {
        assert_eq!(alert(&pending("minos", "a")).unwrap().expose(), "minos Claude needs you: bash approval");
        let mut done = working("m", "d");
        done.group = Group::Done;
        assert!(alert(&done).is_none());
    }
}
