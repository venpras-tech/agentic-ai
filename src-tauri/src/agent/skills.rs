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

use serde::{Deserialize, Serialize};

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
    /// User-defined tags for the UI (KnowledgePanel) to filter by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Glob patterns the skill applies to (triggered by matching file paths).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
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

/// Persisted active-flag state for skills across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillsActiveState {
    /// Map of skill name → active flag.
    active: HashMap<String, bool>,
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

/// Total character cap for the rules buffer (root + nested AGENTS.md +
/// auto-extracted memory). Rules are pinned and must share context budget.
const RULES_TOTAL_CAP: usize = 24000;
/// Maximum directory depth to walk looking for nested `AGENTS.md` files.
const NESTED_RULES_DEPTH: usize = 8;
/// Maximum auto-extracted memory lines kept in `.ai/memory.md`.
const MEMORY_MAX_LINES: usize = 200;

/// Minimal glob matcher supporting `*` (any run, including `/`), `**`
/// (treated as `*`) and `?` (any single char). Used to decide whether a skill's
/// `globs` match an active file path for auto-suggestion.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn match_here(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => match_here(&p[1..], t) || (!t.is_empty() && match_here(p, &t[1..])),
            (Some('?'), Some(_)) => match_here(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => match_here(&p[1..], &t[1..]),
            _ => false,
        }
    }
    match_here(&pattern.chars().collect::<Vec<char>>(), &text.chars().collect::<Vec<char>>())
}

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
        // Nested `AGENTS.md` in subdirectories add on top of the root rules.
        rules_files.extend(find_nested_agents_md(workspace));

        // User-global skills.
        let global_skills = config_dir.join("skills");
        skills_dirs.push(global_skills.clone());
        roots.push(workspace.to_path_buf());
        if global_skills != ws_ai.join("skills") {
            roots.push(global_skills);
        }

        // Load rules (concatenated, with source headers). Root rules come
        // first, then nested AGENTS.md ordered depth-first/alphabetically.
        // Auto-extracted memory is appended as additive context at the end.
        let mut rules = String::new();
        let mut rules_sources = Vec::new();
        let push_rule = |rel: &Path, body: &str, rules: &mut String, sources: &mut Vec<String>| {
            let header_len = format!("### From {}\n", rel.to_string_lossy()).len();
            let needed = header_len + body.trim().len() + 3;
            if rules.len() + needed > RULES_TOTAL_CAP && !rules.is_empty() {
                return;
            }
            sources.push(rel.to_string_lossy().into_owned());
            rules.push_str(&format!(
                "### From {}\n{}\n\n",
                rel.to_string_lossy(),
                body.trim()
            ));
        };
        for f in rules_files {
            let Ok(body) = std::fs::read_to_string(&f) else {
                continue;
            };
            let rel = f.strip_prefix(workspace).unwrap_or(&f);
            let rel_path = rel.to_path_buf();
            push_rule(&rel_path, &body, &mut rules, &mut rules_sources);
        }
        // Load auto-extracted memory as additive context for the model.
        // Labeled distinctly so the model treats it as learned context rather
        // than project conventions.
        if let Some(mem) = read_memory(workspace) {
            if !mem.trim().is_empty() {
                rules_sources.push(".ai/memory.md".to_string());
                rules.push_str(&format!("### Memory (auto-extracted)\n{}\n\n", mem.trim()));
            }
        }

        // Load persisted active flags from disk (survives restarts).
        let persisted_active = Self::load_active_state(workspace);

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
                        // Restore active flag from: (1) in-memory map (same
                        // session), (2) persisted disk state (across restarts),
                        // (3) default true for brand-new skills.
                        let active = self
                            .skills
                            .lock()
                            .unwrap()
                            .get(&skill.name)
                            .map(|s| s.active)
                            .or_else(|| persisted_active.get(&skill.name).copied())
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
                // Persist the active state to disk so it survives restarts.
                let state: HashMap<String, bool> = skills
                    .iter()
                    .map(|(n, s)| (n.clone(), s.active))
                    .collect();
                drop(skills);
                self.save_active_state(&state);
                Ok(())
            }
            None => Err(format!("No skill named `{name}`")),
        }
    }

    /// Save the active-flag state to `.ai/skills-state.json` inside the first
    /// scanned workspace root. Best-effort — errors are silently ignored.
    fn save_active_state(&self, state: &HashMap<String, bool>) {
        let roots = self.roots.lock().unwrap();
        if let Some(ws) = roots.first() {
            let path = ws.join(".ai").join("skills-state.json");
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let payload = SkillsActiveState {
                active: state.clone(),
            };
            if let Ok(json) = serde_json::to_string_pretty(&payload) {
                let _ = std::fs::write(&path, json);
            }
        }
    }

    /// Load the persisted active-flag state from `.ai/skills-state.json`.
    /// Returns an empty map if the file doesn't exist or is malformed.
    fn load_active_state(workspace: &Path) -> HashMap<String, bool> {
        let path = workspace.join(".ai").join("skills-state.json");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<SkillsActiveState>(&s).ok())
            .map(|s| s.active)
            .unwrap_or_default()
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

    /// Sorted union of all skill tags (for the UI to filter by).
    pub fn tags(&self) -> Vec<String> {
        let skills = self.skills.lock().unwrap();
        let mut tags: Vec<String> = skills
            .values()
            .flat_map(|s| s.tags.iter().cloned())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }

    /// Rank available skills against a task prompt (and, when present, the
    /// active file path). Scoring combines:
    /// - a strong bonus (+100) when any of the skill's `globs` match `path`,
    /// - +1 per task keyword (≥3 alnum chars) found in the skill's name,
    ///   description or tags.
    ///
    /// Returns `(score, Skill)` descending by score; only skills with a
    /// non-zero score are returned. Used by the `suggest_skills` tool so the
    /// agent can surface relevant skills it should consider loading.
    pub fn suggest(&self, prompt: &str, path: Option<&str>) -> Vec<(u32, Skill)> {
        let skills = self.skills.lock().unwrap();
        let keywords: Vec<String> = prompt
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.chars().count() >= 3)
            .map(|t| t.to_lowercase())
            .collect();
        let mut out: Vec<(u32, Skill)> = Vec::new();
        for skill in skills.values() {
            let mut score: u32 = 0;
            if let Some(p) = path {
                if skill.globs.iter().any(|g| glob_match(g, p)) {
                    score += 100;
                }
            }
            let hay = format!("{} {} {}", skill.name, skill.description, skill.tags.join(" "))
                .to_lowercase();
            for k in &keywords {
                if hay.contains(k.as_str()) {
                    score += 1;
                }
            }
            if score > 0 {
                out.push((score, skill.clone()));
            }
        }
        out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        out
    }

    /// Append a timestamped learning note to `{ws}/.ai/memory.md`, capped at
    /// `MEMORY_MAX_LINES` lines (oldest dropped first past the cap). The
    /// orchestrator calls this after each completed agent task so learnings
    /// carry across sessions; `memory_content` / `scan` load it back into the
    /// model context on the next session.
    pub fn append_memory(&self, workspace: &Path, note: &str) -> Result<(), String> {
        let note = note.trim();
        if note.is_empty() {
            return Ok(());
        }
        let mem_dir = workspace.join(".ai");
        std::fs::create_dir_all(&mem_dir).map_err(|e| {
            format!("Failed to create `{}`: {e}", mem_dir.display())
        })?;
        let path = mem_dir.join("memory.md");
        let mut lines: Vec<String> = if path.is_file() {
            std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .map(|l| l.to_string())
                .collect()
        } else {
            Vec::new()
        };
        let time = timestamp_str();
        lines.push(format!("- `{time}` {note}"));
        if lines.len() > MEMORY_MAX_LINES {
            let drop = lines.len() - MEMORY_MAX_LINES;
            lines.drain(0..drop);
        }
        std::fs::write(&path, lines.join("\n") + "\n")
            .map_err(|e| format!("Failed to write `{}`: {e}", path.display()))?;
        Ok(())
    }

    /// Return the current auto-extracted memory text (if any) for loading into
    /// the model context on demand.
    pub fn memory_content(&self, workspace: &Path) -> String {
        std::fs::read_to_string(workspace.join(".ai").join("memory.md"))
            .unwrap_or_default()
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
    let parsed = split_frontmatter(&text);
    let name = if parsed.name.is_empty() {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into())
    } else {
        parsed.name
    };
    let source = path
        .strip_prefix(dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
    Ok(Skill {
        name,
        description: parsed.description,
        tags: parsed.tags,
        globs: parsed.globs,
        content: parsed.body,
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

/// Parsed skill frontmatter: name, description, tags, globs, and markdown body.
struct Frontmatter {
    name: String,
    description: String,
    tags: Vec<String>,
    globs: Vec<String>,
    body: String,
}

/// Split `---\nkey: value\n---\nbody` frontmatter. Missing/malformed frontmatter
/// falls back to empty name/description/tags/globs with the whole file as the
/// body. `tags:` and `globs:` accept comma- or space-separated lists.
fn split_frontmatter(text: &str) -> Frontmatter {
    let empty = || Frontmatter {
        name: String::new(),
        description: String::new(),
        tags: Vec::new(),
        globs: Vec::new(),
        body: text.to_string(),
    };
    let Some(rest) = text.strip_prefix("---") else {
        return empty();
    };
    let Some(end) = rest.find("\n---") else {
        return empty();
    };
    let front = &rest[..end];
    let body = rest[end + 4..].trim_start().to_string();
    let mut fm = Frontmatter {
        name: String::new(),
        description: String::new(),
        tags: Vec::new(),
        globs: Vec::new(),
        body,
    };
    for line in front.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name:") {
            fm.name = v.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(v) = line.strip_prefix("description:") {
            fm.description = v.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(v) = line.strip_prefix("tags:") {
            fm.tags = parse_list(v);
        } else if let Some(v) = line.strip_prefix("globs:") {
            fm.globs = parse_list(v);
        }
    }
    fm
}

/// Split a comma/space-separated frontmatter list into trimmed, deduped items.
fn parse_list(v: &str) -> Vec<String> {
    let mut items: Vec<String> = v
        .split([',', ' '])
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    items.sort();
    items.dedup();
    items
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

/// Directories skipped while walking for nested `AGENTS.md` files.
fn is_skipped_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules" | ".git" | "target" | "dist" | ".ai"
    )
}

/// Depth-first walk for nested `AGENTS.md` under `workspace`, returning the
/// files ordered by relative path (root rules are handled by the caller and
/// come first). Skips `node_modules`, `.git`, `target`, `dist`, `.ai`. The
/// `.ai` dir is skipped because its rules/memory are handled separately.
fn find_nested_agents_md(workspace: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > NESTED_RULES_DEPTH {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let Some(file_name) = p.file_name().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            if file_name == "AGENTS.md" && p.is_file() {
                out.push(p);
                continue;
            }
            if p.is_dir() && !is_skipped_dir(&file_name) {
                walk(&p, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(workspace, 0, &mut out);
    // depth-first pre-order already approximates parent-before-child, but sort
    // by relative path for a stable, alphabetical order across branches.
    out.sort_by_key(|p| {
        p.strip_prefix(workspace)
            .unwrap_or(p)
            .to_string_lossy()
            .to_lowercase()
    });
    out
}

/// Local timestamp `YYYY-MM-DD HH:MM:SS` for memory bullets option.
fn timestamp_str() -> String {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// Read `.ai/memory.md` if present; returns `None` when missing/unreadable.
fn read_memory(workspace: &Path) -> Option<String> {
    let path = workspace.join(".ai").join("memory.md");
    if !path.is_file() {
        return None;
    }
    std::fs::read_to_string(&path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let text =
            "---\nname: rust-checks\ndescription: Typecheck + test\n---\n# Body\ncargo check";
        let fm = split_frontmatter(text);
        assert_eq!(fm.name, "rust-checks");
        assert_eq!(fm.description, "Typecheck + test");
        assert!(fm.body.starts_with("# Body"));
    }

    #[test]
    fn missing_frontmatter_uses_whole_body() {
        let fm = split_frontmatter("# no meta\ncontent");
        assert_eq!(fm.name, "");
        assert_eq!(fm.description, "");
        assert_eq!(fm.body, "# no meta\ncontent");
    }

    #[test]
    fn parses_tags_and_globs_lists() {
        let text = "---\nname: t\ndescription: D\ntags: rust, testing, cli\nglobs: \"**/*.rs\" **/Cargo.toml\n---\n# T\nbody";
        let fm = split_frontmatter(text);
        assert_eq!(fm.tags, vec!["cli", "rust", "testing"]);
        assert_eq!(fm.globs, vec!["**/*.rs", "**/Cargo.toml"]);
    }

    #[test]
    fn tags_and_globs_default_empty_for_old_skills() {
        let fm = split_frontmatter("---\nname: old\ndescription: Old\n---\nbody");
        assert!(fm.tags.is_empty());
        assert!(fm.globs.is_empty());
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
    fn active_flags_persist_across_scans() {
        let dir = std::env::temp_dir().join(format!("ai-skill-persist-{}", std::process::id()));
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

        // First scan: disable alpha.
        let ks = KnowledgeState::default();
        ks.scan(&ws, &dir.join("cfg")).unwrap();
        ks.set_active("alpha", false).unwrap();

        // Verify the state file was written.
        let state_path = ws.join(".ai").join("skills-state.json");
        assert!(state_path.is_file());
        let state_content = std::fs::read_to_string(&state_path).unwrap();
        assert!(state_content.contains("\"alpha\""));
        assert!(state_content.contains("false"));

        // Simulate restart: new KnowledgeState, re-scan.
        let ks2 = KnowledgeState::default();
        let report2 = ks2.scan(&ws, &dir.join("cfg")).unwrap();
        let alpha = report2.skills.iter().find(|s| s.name == "alpha").unwrap();
        let beta = report2.skills.iter().find(|s| s.name == "beta").unwrap();
        assert!(!alpha.active, "alpha should remain disabled after restart");
        assert!(beta.active, "beta should remain active after restart");

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

    #[test]
    fn nested_agents_md_append_after_root_in_scan() {
        let dir = std::env::temp_dir().join(format!("ai-nested-{}", std::process::id()));
        let ws = dir.join("ws");
        let api = ws.join("src").join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(&ws.join("AGENTS.md"), "ROOT RULES").unwrap();
        std::fs::write(&api.join("AGENTS.md"), "API SPECIFIC").unwrap();

        let ks = KnowledgeState::default();
        let report = ks.scan(&ws, &dir.join("cfg")).unwrap();

        let root_pos = report.rules.find("ROOT RULES").unwrap();
        let api_pos = report.rules.find("API SPECIFIC").unwrap();
        assert!(root_pos < api_pos, "root rules must come before nested");
        // The nested file is labeled with its real relative path, which uses
        // the OS path separator (on Windows: `src\api\AGENTS.md`).
        let rel_api = api
            .join("AGENTS.md")
            .strip_prefix(&ws)
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(rel_api.contains("api"), "sanity: nested rel path computed");
        let rel_header = report
            .rules
            .find(&format!("### From {rel_api}"))
            .expect("nested rules must carry its relative path header");
        assert!(rel_header < api_pos);
        assert!(report.rules_sources.contains(&rel_api));
        assert!(report.rules_sources.contains(&"AGENTS.md".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nested_agents_md_skips_node_modules_and_git() {
        let dir = std::env::temp_dir().join(format!("ai-nested-skip-{}", std::process::id()));
        let ws = dir.join("ws");
        let nm = ws.join("node_modules").join("dep");
        let git = ws.join(".git");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(&nm.join("AGENTS.md"), "SHOULD NOT APPEAR").unwrap();
        std::fs::write(&git.join("AGENTS.md"), "SHOULD NOT APPEAR EITHER").unwrap();
        std::fs::write(&ws.join("AGENTS.md"), "ROOT").unwrap();

        let found = find_nested_agents_md(&ws);
        // `node_modules` / `.git` AGENTS.md must be skipped; the root file is
        // (correctly) still discovered.
        assert_eq!(found.len(), 1, "only the root AGENTS.md should be found");
        assert!(
            !found.iter().any(|p| {
                let s = p.to_string_lossy();
                s.contains("node_modules") || s.contains(".git")
            }),
            "skipped dirs must be excluded"
        );

        // Also verify through scan: only the root rules surface.
        let ks = KnowledgeState::default();
        let report = ks.scan(&ws, &dir.join("cfg")).unwrap();
        assert!(!report.rules.contains("SHOULD NOT APPEAR"));
        assert!(report.rules.contains("ROOT"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tags_helper_returns_sorted_union_across_skills() {
        let dir = std::env::temp_dir().join(format!("ai-tags-{}", std::process::id()));
        let ws = dir.join("ws");
        let skills = ws.join(".ai").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("a.md"),
            "---\nname: a\ndescription: A\ntags: rust, cli\n---\n# A\n",
        )
        .unwrap();
        std::fs::write(
            skills.join("b.md"),
            "---\nname: b\ndescription: B\ntags: testing, rust\n---\n# B\n",
        )
        .unwrap();

        let ks = KnowledgeState::default();
        ks.scan(&ws, &dir.join("cfg")).unwrap();
        assert_eq!(ks.tags(), vec!["cli", "rust", "testing"]);

        let report = ks.report();
        for s in &report.skills {
            if s.name == "a" {
                assert_eq!(s.tags, vec!["cli", "rust"]);
            } else if s.name == "b" {
                assert_eq!(s.tags, vec!["rust", "testing"]);
            }
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_match_supports_star_question_and_slash() {
        assert!(glob_match("*.rs", "src/agent/main.rs"));
        assert!(glob_match("**/*.tsx", "components/App.tsx"));
        assert!(glob_match("src/?.rs", "src/a.rs"));
        assert!(!glob_match("*.rs", "src/agent/main.ts"));
        assert!(!glob_match("python/*.py", "rust/main.rs"));
        assert!(glob_match("python/*.py", "python/parse.py"));
    }

    #[test]
    fn suggest_ranks_glob_hit_above_keyword_only() {
        let dir = std::env::temp_dir().join(format!("ai-sug-{}", std::process::id()));
        let ws = dir.join("ws");
        let skills = ws.join(".ai").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("rust.md"),
            "---\nname: rust-guidelines\ndescription: Rust project conventions\nglobs: \"**/*.rs\"\n---\n# Rust\n",
        )
        .unwrap();
        std::fs::write(
            skills.join("tables.md"),
            "---\nname: sql-tables\ndescription: Database schema design\ntags: sql, database\n---\n# SQL\n",
        )
        .unwrap();
        let ks = KnowledgeState::default();
        ks.scan(&ws, &dir.join("cfg")).unwrap();

        // A Rust file path: rust-guidelines matches by glob (+ keywords) and
        // outranks the sql-tables keyword-only match.
        let ranked = ks.suggest(
            "write rust and schema code for the parser",
            Some("src/parse.rs"),
        );
        assert!(!ranked.is_empty());
        assert_eq!(ranked[0].1.name, "rust-guidelines");
        assert_eq!(ranked[1].1.name, "sql-tables", "keyword-only still suggested");

        // A SQL-ish prompt with no file: only sql-tables should match.
        let ranked2 = ks.suggest("design the users table schema", None);
        assert!(ranked2.iter().all(|(_, s)| s.name == "sql-tables"));

        // No overlap at all -> empty.
        let ranked3 = ks.suggest("xyz", None);
        assert!(ranked3.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_memory_truncates_oldest_beyond_cap() {
        let dir = std::env::temp_dir().join(format!("ai-mem-{}", std::process::id()));
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        let ks = KnowledgeState::default();
        for i in 0..250 {
            ks.append_memory(&ws, &format!("learning #{i}")).unwrap();
        }

        let content = ks.memory_content(&ws);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), MEMORY_MAX_LINES);
        // Oldest entries (0..50) are dropped; the newest survive.
        assert!(lines.first().unwrap().contains("learning #50"));
        assert!(lines.last().unwrap().contains("learning #249"));
        assert!(!content.contains("learning #0"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_pulls_memory_into_rules_buffer() {
        let dir = std::env::temp_dir().join(format!("ai-mem-scan-{}", std::process::id()));
        let ws = dir.join("ws");
        let api = ws.join("src");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(&ws.join("AGENTS.md"), "ROOT RULES").unwrap();

        let ks = KnowledgeState::default();
        ks.append_memory(&ws, "api uses rust conv").unwrap();

        let report = ks.scan(&ws, &dir.join("cfg")).unwrap();
        // Memory is loaded into the pinned rules/context buffer.
        let memory_header = report
            .rules
            .find("### Memory (auto-extracted)")
            .expect("memory must be labeled in the rules buffer");
        assert!(report.rules.contains("api uses rust conv"));
        // Root rules come before memory.
        assert!(report.rules.find("ROOT RULES").unwrap() < memory_header);
        assert!(report.rules_sources.contains(&".ai/memory.md".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }
}
