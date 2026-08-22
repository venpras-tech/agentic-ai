//! Skills & Rules: injectable, project-scoped instruction bundles.
//!
//! * **Rules** — always-active project conventions loaded from `AGENTS.md`,
//!   `.cursorrules`, `CLAUDE.md` and every `*.md` under
//!   `{workspace}/.ai/rules/`. Rules are pinned into the context as a single
//!   role-scoped buffer and can never be evicted.
//! * **Skills** — reusable capability packs (workflows, tool recipes,
//!   domain knowledge) stored as Markdown files under `{workspace}/.ai/skills/`
//!   (and a user-global `{config_dir}/skills/`). Each skill has a `name` and
//!   `description` in YAML frontmatter. Skills are *opt-out*: every available
//!   skill is pinned into the context (within a character budget) so the model
//!   automatically works per the project's skills. The user can toggle any
//!   skill off from the UI; the model can also call the `read_skill` tool to
//!   load any skill's full text on demand.
//!
//! File format:
//! ```markdown
//! ---
//! name: rust-checks
//! description: How to typecheck and test this Rust workspace
//! ---
//! # Rust checks
//! Always run `cargo check` and `cargo test` after editing `src-tauri/`.
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

/// A loaded skill (frontmatter + body).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub content: String,
    pub source: String,
    /// Whether the user has toggled this skill into the active context.
    pub active: bool,
    /// Absolute backing path — the `.md` file itself, or the `SKILL.md`
    /// inside a folder-format skill. Used by `uninstall`; not serialized.
    #[serde(skip)]
    pub abs_path: PathBuf,
}

/// Snapshot for the UI: rules text + every available skill.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeReport {
    pub rules: String,
    pub rules_sources: Vec<String>,
    pub skills: Vec<Skill>,
}

/// Long-lived knowledge state managed by Tauri.
pub struct KnowledgeState {
    pub skills: Mutex<HashMap<String, Skill>>,
    pub rules: Mutex<String>,
    pub rules_sources: Mutex<Vec<String>>,
    /// Directories scanned for skills, in load order.
    roots: Mutex<Vec<PathBuf>>,
}

/// Per-skill body character cap injected into the pinned context buffer.
/// Longer skills are clipped with a note pointing at `read_skill`.
const SKILL_BODY_CAP: usize = 3000;
/// Total character cap for all active skills in the pinned context buffer.
const SKILL_TOTAL_CAP: usize = 24000;

impl Default for KnowledgeState {
    fn default() -> Self {
        Self {
            skills: Mutex::new(HashMap::new()),
            rules: Mutex::new(String::new()),
            rules_sources: Mutex::new(Vec::new()),
            roots: Mutex::new(Vec::new()),
        }
    }
}

impl KnowledgeState {
    /// (Re)scan the given workspace root + config dir. Idempotent: same-named
    /// skills are replaced, active flags are preserved across rescans.
    pub fn scan(&self, workspace: &Path, config_dir: &Path) -> Result<KnowledgeReport, String> {
        let mut roots: Vec<PathBuf> = Vec::new();
        let mut rules_files: Vec<PathBuf> = Vec::new();
        let mut skills_dirs: Vec<PathBuf> = Vec::new();

        // Workspace-level knowledge.
        let ws_ai = workspace.join(".ai");
        skills_dirs.push(ws_ai.join("skills"));
        let ws_rules = ws_ai.join("rules");
        if ws_rules.is_dir() {
            rules_files.extend(list_md(&ws_rules));
        }
        for name in ["AGENTS.md", ".cursorrules", "CLAUDE.md"] {
            let p = workspace.join(name);
            if p.is_file() {
                rules_files.push(p);
            }
        }

        // User-global skills.
        let global_skills = config_dir.join("skills");
        skills_dirs.push(global_skills.clone());
        roots.push(workspace.to_path_buf());
        if global_skills != ws_ai.join("skills") {
            roots.push(global_skills);
        }

        // Load rules (concatenated, with source headers).
        let mut rules = String::new();
        let mut rules_sources = Vec::new();
        for f in rules_files {
            let body = std::fs::read_to_string(&f)
                .map_err(|e| format!("Failed to read rules file `{}`: {e}", f.display()))?;
            let rel = f.strip_prefix(workspace).unwrap_or(&f);
            rules_sources.push(rel.to_string_lossy().into_owned());
            rules.push_str(&format!(
                "### From {}\n{}\n\n",
                rel.to_string_lossy(),
                body.trim()
            ));
        }

        // Load skills. Two layouts are supported inside each skills dir:
        //   flat    `{dir}/name.md`
        //   folder  `{dir}/name/SKILL.md`   (scripts/data may sit alongside)
        let mut skills: HashMap<String, Skill> = HashMap::new();
        for dir in &skills_dirs {
            if !dir.is_dir() {
                continue;
            }
            let entries = std::fs::read_dir(dir)
                .map_err(|e| format!("Failed to list `{}`: {e}", dir.display()))?;
            for entry in entries {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                let parsed: Result<Skill, String> = if path.is_dir() {
                    let skill_md = path.join("SKILL.md");
                    if skill_md.is_file() {
                        parse_skill(&skill_md, dir)
                    } else {
                        continue; // plain folder, not a skill
                    }
                } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                    parse_skill(&path, dir)
                } else {
                    continue;
                };
                match parsed {
                    Ok(skill) => {
                        // Preserve the previous active flag across rescans;
                        // newly discovered skills are auto-active (opt-out).
                        let active = self
                            .skills
                            .lock()
                            .unwrap()
                            .get(&skill.name)
                            .map(|s| s.active)
                            .unwrap_or(true);
                        skills.insert(skill.name.clone(), Skill { active, ..skill });
                    }
                    Err(_e) => {
                        // skip malformed files silently
                    }
                }
            }
        }

        *self.skills.lock().unwrap() = skills.clone();
        *self.rules.lock().unwrap() = rules.clone();
        *self.rules_sources.lock().unwrap() = rules_sources.clone();
        *self.roots.lock().unwrap() = roots;

        Ok(KnowledgeReport {
            rules: rules.clone(),
            rules_sources: rules_sources.clone(),
            skills: skills.values().cloned().collect(),
        })
    }

    pub fn set_active(&self, name: &str, active: bool) -> Result<(), String> {
        let mut skills = self.skills.lock().unwrap();
        match skills.get_mut(name) {
            Some(s) => {
                s.active = active;
                Ok(())
            }
            None => Err(format!("No skill named `{name}`")),
        }
    }

    /// Install a skill from a source path — a `*.md` file or a folder
    /// containing `SKILL.md` — into the workspace (`{ws}/.ai/skills`) or the
    /// user-global (`{config}/skills`) skills directory. Fails if the
    /// destination already exists. Call `scan` afterwards to pick it up.
    pub fn install(
        workspace: &Path,
        config_dir: &Path,
        source: &Path,
        global: bool,
    ) -> Result<String, String> {
        let is_md_file = source.is_file() && source.extension().map(|e| e == "md").unwrap_or(false);
        let skill_md = source.join("SKILL.md");
        let is_folder = source.is_dir() && skill_md.is_file();
        if !is_md_file && !is_folder {
            return Err("Source must be a .md file or a folder containing SKILL.md".to_string());
        }

        let dest_dir = if global {
            config_dir.join("skills")
        } else {
            workspace.join(".ai").join("skills")
        };
        std::fs::create_dir_all(&dest_dir)
            .map_err(|e| format!("Failed to create `{}`: {e}", dest_dir.display()))?;

        let file_name = source
            .file_name()
            .ok_or_else(|| "Invalid source path".to_string())?;
        let dest = dest_dir.join(file_name);
        if dest.exists() {
            return Err(format!(
                "`{}` already exists in the target skills directory",
                file_name.to_string_lossy()
            ));
        }

        if is_folder {
            copy_dir_recursive(source, &dest)?;
        } else {
            std::fs::copy(source, &dest).map_err(|e| format!("Copy failed: {e}"))?;
        }

        // Validate the installed copy parses (also gives the real name).
        parse_skill(
            &if is_folder {
                dest.join("SKILL.md")
            } else {
                dest
            },
            &dest_dir,
        )
        .map(|s| s.name)
    }

    /// Delete a skill's backing file/folder from disk. Returns the removed
    /// path. Call `scan` afterwards so the in-memory map matches.
    pub fn uninstall(&self, name: &str) -> Result<PathBuf, String> {
        let path = {
            let skills = self.skills.lock().unwrap();
            skills
                .get(name)
                .ok_or_else(|| format!("No skill named `{name}`"))?
                .abs_path
                .clone()
        };
        // Folder-format skills have abs_path = `<dir>/SKILL.md`; removing
        // just the file would orphan bundled scripts, so drop the whole
        // folder. Flat skills are plain files removed directly.
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("Failed to stat `{}`: {e}", path.display()))?;
        let is_skill_md = path
            .file_name()
            .map(|f| f.eq_ignore_ascii_case("SKILL.md"))
            .unwrap_or(false);
        let target = if meta.is_dir() || is_skill_md {
            path.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| path.clone())
        } else {
            path.clone()
        };
        if target.is_dir() {
            std::fs::remove_dir_all(&target).map_err(|e| format!("Remove failed: {e}"))?;
        } else {
            std::fs::remove_file(&target).map_err(|e| format!("Remove failed: {e}"))?;
        }
        Ok(target)
    }

    /// Snapshot for the UI.
    pub fn report(&self) -> KnowledgeReport {
        KnowledgeReport {
            rules: self.rules.lock().unwrap().clone(),
            rules_sources: self.rules_sources.lock().unwrap().clone(),
            skills: self.skills.lock().unwrap().values().cloned().collect(),
        }
    }

    /// Fetch the full content of any available skill by name (active or not).
    /// Used by the `read_skill` tool so the model can load the *untruncated*
    /// text of a skill that was clipped from the pinned context.
    pub fn get_skill(&self, name: &str) -> Option<Skill> {
        self.skills.lock().unwrap().get(name).cloned()
    }

    /// Names of every available skill, sorted (for error messages).
    pub fn skill_names(&self) -> Vec<String> {
        let skills = self.skills.lock().unwrap();
        let mut names: Vec<String> = skills.keys().cloned().collect();
        names.sort();
        names
    }

    /// Render all *active* skills as one pinned "skill" buffer.
    ///
    /// Skills are auto-active on first discovery, so this includes every
    /// available skill by default. Long skills are clipped per-skill and a
    /// total budget is enforced so the pinned buffer can never blow the KV
    /// cache; clipped skills are listed in a footer pointing at `read_skill`.
    pub fn active_skills_content(&self) -> String {
        let skills = self.skills.lock().unwrap();
        let mut active: Vec<&Skill> = skills.values().filter(|s| s.active).collect();
        active.sort_by(|a, b| a.name.cmp(&b.name));

        let mut out = String::new();
        let mut truncated: Vec<String> = Vec::new();
        for s in active {
            let body = s.content.trim();
            let shown = if body.chars().count() > SKILL_BODY_CAP {
                truncated.push(s.name.clone());
                let mut clipped: String = body.chars().take(SKILL_BODY_CAP).collect();
                clipped.push_str(&format!(
                    "\n\n[Skill `{}` was truncated to save context — call read_skill(\"{}\") for its full text before applying it.]",
                    s.name, s.name
                ));
                clipped
            } else {
                body.to_string()
            };
            if out.len() + shown.len() > SKILL_TOTAL_CAP {
                truncated.push(s.name.clone());
                continue;
            }
            out.push_str(&format!("# Skill: {}\n{}\n\n", s.name, shown));
        }
        truncated.sort();
        truncated.dedup();
        if !truncated.is_empty() {
            out.push_str(&format!(
                "[Not fully loaded (context budget): {}. Call read_skill(\"<name>\") to load the full text of one of these skills before using it.]\n",
                truncated.join(", ")
            ));
        }
        out
    }
}

/// Parse a skill file: optional YAML frontmatter + markdown body.
fn parse_skill(path: &Path, dir: &Path) -> Result<Skill, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read `{}`: {e}", path.display()))?;
    let (name, description, body) = split_frontmatter(&text);
    let name = if name.is_empty() {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into())
    } else {
        name
    };
    let source = path
        .strip_prefix(dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
    Ok(Skill {
        name,
        description,
        content: body,
        source,
        active: true,
        abs_path: path.to_path_buf(),
    })
}

/// Recursively copy a directory tree (used to install folder-format skills).
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir failed: {e}"))?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| format!("copy failed: {e}"))?;
        }
    }
    Ok(())
}

/// Split `---\nkey: value\n---\nbody` frontmatter. Missing/malformed frontmatter
/// falls back to an empty name/description with the whole file as the body.
fn split_frontmatter(text: &str) -> (String, String, String) {
    let Some(rest) = text.strip_prefix("---") else {
        return (String::new(), String::new(), text.to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (String::new(), String::new(), text.to_string());
    };
    let front = &rest[..end];
    let body = rest[end + 4..].trim_start().to_string();
    let mut name = String::new();
    let mut description = String::new();
    for line in front.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name:") {
            name = v.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(v) = line.strip_prefix("description:") {
            description = v.trim().trim_matches('"').trim_matches('\'').to_string();
        }
    }
    (name, description, body)
}

fn list_md(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().map(|e| e == "md").unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let text =
            "---\nname: rust-checks\ndescription: Typecheck + test\n---\n# Body\ncargo check";
        let (name, desc, body) = split_frontmatter(text);
        assert_eq!(name, "rust-checks");
        assert_eq!(desc, "Typecheck + test");
        assert!(body.starts_with("# Body"));
    }

    #[test]
    fn missing_frontmatter_uses_whole_body() {
        let (name, desc, body) = split_frontmatter("# no meta\ncontent");
        assert_eq!(name, "");
        assert_eq!(desc, "");
        assert_eq!(body, "# no meta\ncontent");
    }

    #[test]
    fn newly_discovered_skills_are_active_by_default() {
        let dir = std::env::temp_dir().join(format!("ai-skill-default-{}", std::process::id()));
        let ws = dir.join("ws");
        let skills = ws.join(".ai").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("alpha.md"),
            "---\nname: alpha\ndescription: A\n---\n# Alpha\nbody\n",
        )
        .unwrap();
        std::fs::write(
            skills.join("beta.md"),
            "---\nname: beta\ndescription: B\n---\n# Beta\nbody\n",
        )
        .unwrap();

        let ks = KnowledgeState::default();
        let report = ks.scan(&ws, &dir.join("cfg")).unwrap();
        assert_eq!(report.skills.len(), 2);
        assert!(report.skills.iter().all(|s| s.active));

        let pinned = ks.active_skills_content();
        assert!(pinned.contains("# Skill: alpha"));
        assert!(pinned.contains("# Skill: beta"));

        ks.set_active("alpha", false).unwrap();
        let after = ks.active_skills_content();
        assert!(!after.contains("# Skill: alpha"));
        assert!(after.contains("# Skill: beta"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn get_skill_returns_full_content_and_caps_clip_long_skills() {
        let dir = std::env::temp_dir().join(format!("ai-skill-cap-{}", std::process::id()));
        let ws = dir.join("ws");
        let skills = ws.join(".ai").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        let long_body = format!("# Long\n{}", "word ".repeat(4000));
        std::fs::write(
            skills.join("big.md"),
            format!("---\nname: big\ndescription: Big\n---\n{long_body}\n"),
        )
        .unwrap();

        let ks = KnowledgeState::default();
        ks.scan(&ws, &dir.join("cfg")).unwrap();

        let full = ks.get_skill("big").unwrap();
        assert!(full.content.contains("word ".repeat(4000).as_str()));

        let pinned = ks.active_skills_content();
        assert!(pinned.contains("was truncated to save context"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn folder_skills_with_skill_md_are_discovered() {
        let dir = std::env::temp_dir().join(format!("ai-skill-folder-{}", std::process::id()));
        let ws = dir.join("ws");
        let tools = ws.join(".ai").join("skills").join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(
            tools.join("SKILL.md"),
            "---\nname: tools\ndescription: Folder skill\n---\n# Tools\nuse run.sh",
        )
        .unwrap();
        std::fs::write(tools.join("run.sh"), "echo hi").unwrap();
        // A plain folder without SKILL.md must be ignored.
        std::fs::create_dir_all(ws.join(".ai").join("skills").join("notaskill")).unwrap();

        let ks = KnowledgeState::default();
        let report = ks.scan(&ws, &dir.join("cfg")).unwrap();

        assert_eq!(report.skills.len(), 1);
        let s = &report.skills[0];
        assert_eq!(s.name, "tools");
        assert_eq!(s.source.replace('\\', "/"), "tools/SKILL.md");

        // Uninstall removes the whole folder (scripts included).
        let removed = ks.uninstall("tools").unwrap();
        assert!(removed.ends_with("tools"));
        assert!(!ws.join(".ai").join("skills").join("tools").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn install_and_uninstall_roundtrip_global_and_workspace() {
        let dir = std::env::temp_dir().join(format!("ai-skill-inst-{}", std::process::id()));
        let ws = dir.join("ws");
        let cfg = dir.join("cfg");
        std::fs::create_dir_all(&ws).unwrap();

        // Flat .md file -> workspace scope.
        let src_md = dir.join("solo.md");
        std::fs::write(
            &src_md,
            "---\nname: solo\ndescription: S\n---\n# Solo\nbody",
        )
        .unwrap();
        let name1 = KnowledgeState::install(&ws, &cfg, &src_md, false).unwrap();
        assert_eq!(name1, "solo");
        assert!(ws.join(".ai").join("skills").join("solo.md").is_file());

        // Folder w/ SKILL.md + data file -> global scope.
        let src_folder = dir.join("packy");
        std::fs::create_dir_all(src_folder.join("data")).unwrap();
        std::fs::write(
            src_folder.join("SKILL.md"),
            "---\nname: packy\ndescription: P\n---\n# Packy",
        )
        .unwrap();
        std::fs::write(src_folder.join("data").join("x.txt"), "hi").unwrap();
        let name2 = KnowledgeState::install(&ws, &cfg, &src_folder, true).unwrap();
        assert_eq!(name2, "packy");
        assert!(cfg
            .join("skills")
            .join("packy")
            .join("data")
            .join("x.txt")
            .is_file());

        // Duplicate install is refused.
        assert!(KnowledgeState::install(&ws, &cfg, &src_folder, true).is_err());

        // Both are discoverable after a scan; uninstall cleans them up.
        let ks = KnowledgeState::default();
        let report = ks.scan(&ws, &cfg).unwrap();
        let mut names: Vec<String> = report.skills.iter().map(|s| s.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["packy".to_string(), "solo".to_string()]);

        ks.uninstall("solo").unwrap();
        ks.uninstall("packy").unwrap();
        assert!(!ws.join(".ai").join("skills").join("solo.md").exists());
        assert!(!cfg.join("skills").join("packy").exists());

        std::fs::remove_dir_all(&dir).ok();
    }
}
