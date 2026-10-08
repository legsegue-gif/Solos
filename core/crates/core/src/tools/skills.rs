//! `skill_install`: puts a skill from GitHub into the skills folder, the way
//! Settings does, and hands the model its instructions in the same turn.

use super::{object_schema, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::skills;
use async_trait::async_trait;
use serde_json::{json, Value};
use solos_api::CoreError;

/// The instructions come back whole up to here; a longer `SKILL.md` is read
/// with `file_read`.
const MAX_INSTRUCTIONS: usize = 20_000;

pub struct SkillInstall;

#[async_trait]
impl Tool for SkillInstall {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill_install".into(),
            description: format!(
                "Install a skill from GitHub into {}/, or update it if it is there. Give the link the user gave: a repository, or the folder in one that holds the SKILL.md. Returns where the skill went and its SKILL.md; then do what it says it needs (packages to install, for example).",
                skills::guest_dir()
            ),
            schema: object_schema(
                json!({
                    "source": {
                        "type": "string",
                        "description": "https://github.com/<owner>/<repo>, or …/tree/<branch>/<folder> for one skill among several."
                    }
                }),
                &["source"],
            ),
            parallel: false,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let Some(source) = input.get("source").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) else {
            return ToolOutput::error("`source` is required: a GitHub link.");
        };
        let workspace = ctx.sandbox.workspace_dir();
        let done = tokio::select! {
            r = skills::install(source, &workspace) => r,
            _ = ctx.cancel.cancelled() => return ToolOutput::error("Stopped."),
        };
        match done {
            Ok(done) => ToolOutput::ok(report(&done)),
            Err(e) => ToolOutput::error(failure(&e)),
        }
    }
}

fn report(done: &skills::Installed) -> String {
    let path = format!("{}/{}", skills::guest_dir(), done.folder);
    let mut instructions = super::clip(&done.instructions, MAX_INSTRUCTIONS);
    if instructions.len() < done.instructions.len() {
        instructions.push_str(&format!("\n[Read the whole file with file_read: {path}/{}]", skills::FILE));
    }
    format!(
        "Installed the skill `{}`{} in {path}: {}. It is listed with the installed skills from the user's next message on.\n\n{path}/{}:\n{instructions}",
        done.folder,
        if done.replaced { " (replacing the copy that was there)" } else { "" },
        files_line(&done.files),
        skills::FILE,
    )
}

fn files_line(files: &[String]) -> String {
    const SHOWN: usize = 30;
    let mut line = files.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    if files.len() > SHOWN {
        line.push_str(&format!(" and {} more", files.len() - SHOWN));
    }
    line
}

fn failure(e: &CoreError) -> String {
    match e {
        CoreError::NotASkillSource { source_text } => {
            format!("Not a GitHub link: {source_text}. Give https://github.com/<owner>/<repo>, or a link to the folder that holds the SKILL.md.")
        }
        CoreError::NoSingleSkill { source_text, candidates } if candidates.is_empty() => {
            format!("There is no SKILL.md in {source_text}, so it is not a skill.")
        }
        CoreError::NoSingleSkill { source_text, candidates } => format!(
            "{source_text} holds {} skills. Install one by its folder's link:\n{}",
            candidates.len(),
            candidates.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n")
        ),
        CoreError::Http { status, detail } => format!("GitHub answered {status}: {detail}"),
        other => format!("The skill could not be installed: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_names_the_folder_files_and_instructions() {
        let done = skills::Installed {
            folder: "12306-skill".into(),
            files: vec!["SKILL.md".into(), "scripts/q.py".into()],
            replaced: true,
            instructions: "---\nname: 12306-skill\n---\npip install requests".into(),
        };
        let text = report(&done);
        assert!(text.starts_with("Installed the skill `12306-skill` (replacing the copy that was there) in /solos/ws/skills/12306-skill: SKILL.md, scripts/q.py."));
        assert!(text.ends_with("/solos/ws/skills/12306-skill/SKILL.md:\n---\nname: 12306-skill\n---\npip install requests"));
    }

    #[test]
    fn several_skills_are_listed_by_link() {
        let e = CoreError::NoSingleSkill { source_text: "https://github.com/o/r".into(), candidates: vec!["https://github.com/o/r/tree/HEAD/a".into()] };
        assert!(failure(&e).contains("- https://github.com/o/r/tree/HEAD/a"));
    }
}
