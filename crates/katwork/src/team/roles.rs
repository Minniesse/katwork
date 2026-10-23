use super::{Appearance, Autonomy, DotRole, NewDot};
use crate::types::{Locale, ModelTier};

pub fn template(role: &DotRole, locale: Locale) -> NewDot {
    let (en, th, avatar, instructions) = match role {
        DotRole::Lead => (
            "Lead",
            "หัวหน้าทีม",
            "compass",
            "Coordinate the Dots and clarify the user's goal.",
        ),
        DotRole::Researcher => (
            "Researcher",
            "นักวิจัย",
            "magnifier",
            "Gather research, cite sources, and flag uncertainty.",
        ),
        DotRole::Writer => (
            "Writer",
            "นักเขียน",
            "pen",
            "Write clear drafts suited to the user's audience.",
        ),
        DotRole::Analyst => (
            "Analyst",
            "นักวิเคราะห์",
            "chart",
            "Analyze data, check assumptions, and explain findings.",
        ),
        DotRole::Scheduler => (
            "Scheduler",
            "เลขา",
            "calendar",
            "Organize schedules and clarify timing and dependencies.",
        ),
        DotRole::Reviewer => (
            "Reviewer",
            "ผู้ตรวจ",
            "check",
            "Review work for accuracy, completeness, and risks.",
        ),
        DotRole::Custom(name) => (
            name.as_str(),
            name.as_str(),
            "dot",
            "Help with the user's task and ask when clarification is needed.",
        ),
    };
    NewDot {
        name: match locale {
            Locale::En => en,
            Locale::Th => th,
        }
        .into(),
        role: role.clone(),
        persona: String::new(),
        instructions: instructions.into(),
        tool_allowlist: vec!["ask_user".into(), "read_thread".into()],
        model_tier: ModelTier::Standard,
        autonomy: Autonomy::AskFirst,
        appearance: Appearance {
            avatar: avatar.into(),
            accent: "blue".into(),
        },
    }
}

pub fn default_roster(locale: Locale) -> Vec<NewDot> {
    [
        DotRole::Lead,
        DotRole::Researcher,
        DotRole::Writer,
        DotRole::Analyst,
        DotRole::Scheduler,
        DotRole::Reviewer,
    ]
    .iter()
    .map(|role| template(role, locale))
    .collect()
}
