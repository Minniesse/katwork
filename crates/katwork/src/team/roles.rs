use super::{Appearance, Autonomy, KatRole, NewKat};
use crate::types::{Locale, ModelTier};

pub fn template(role: &KatRole, locale: Locale) -> NewKat {
    let (en, th, avatar, instructions) = match role {
        KatRole::Lead => (
            "Lead",
            "หัวหน้าทีม",
            "compass",
            "Coordinate the Kats and clarify the user's goal.",
        ),
        KatRole::Researcher => (
            "Researcher",
            "นักวิจัย",
            "magnifier",
            "Gather research, cite sources, and flag uncertainty.",
        ),
        KatRole::Writer => (
            "Writer",
            "นักเขียน",
            "pen",
            "Write clear drafts suited to the user's audience.",
        ),
        KatRole::Analyst => (
            "Analyst",
            "นักวิเคราะห์",
            "chart",
            "Analyze data, check assumptions, and explain findings.",
        ),
        KatRole::Scheduler => (
            "Scheduler",
            "เลขา",
            "calendar",
            "Organize schedules and clarify timing and dependencies.",
        ),
        KatRole::Reviewer => (
            "Reviewer",
            "ผู้ตรวจ",
            "check",
            "Review work for accuracy, completeness, and risks.",
        ),
        KatRole::Custom(name) => (
            name.as_str(),
            name.as_str(),
            "kat",
            "Help with the user's task and ask when clarification is needed.",
        ),
    };
    // Per message, not per account: people who read Thai often write to a Kat in English.
    let (name, reply) = match locale {
        Locale::En => (en, "Reply in the language of the person's latest message."),
        Locale::Th => (th, "ตอบเป็นภาษาเดียวกับข้อความล่าสุดของคนที่คุยด้วย"),
    };
    NewKat {
        name: name.into(),
        role: role.clone(),
        persona: String::new(),
        instructions: format!("{instructions} {reply}"),
        tool_allowlist: vec!["ask_user".into(), "read_thread".into()],
        model_tier: ModelTier::Standard,
        autonomy: Autonomy::AskFirst,
        appearance: Appearance {
            avatar: avatar.into(),
            accent: "blue".into(),
        },
        max_concurrency: if *role == KatRole::Lead { 3 } else { 1 },
    }
}

pub fn default_roster(locale: Locale) -> Vec<NewKat> {
    [
        KatRole::Lead,
        KatRole::Researcher,
        KatRole::Writer,
        KatRole::Analyst,
        KatRole::Scheduler,
        KatRole::Reviewer,
    ]
    .iter()
    .map(|role| template(role, locale))
    .collect()
}
