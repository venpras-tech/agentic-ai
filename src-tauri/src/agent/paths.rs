use std::path::PathBuf;

pub fn resolve_tool_path(workspaces: &[PathBuf], raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(ws) = workspaces.first() {
        if let Some(rest) = workspace_suffix(trimmed) {
            if rest.is_empty() {
                return ws.clone();
            }
            return ws.join(rest);
        }
    }

    let p = PathBuf::from(trimmed);
    if p.is_absolute() {
        p
    } else if let Some(ws) = workspaces.first() {
        ws.join(p)
    } else {
        p
    }
}

fn workspace_suffix(raw: &str) -> Option<&str> {
    let mut s = raw;
    if s.starts_with('/') || s.starts_with('\\') {
        s = s.trim_start_matches(['/', '\\']);
    }
    let lower = s.to_ascii_lowercase();
    const PREFIXES: [&str; 2] = ["workspace/", "workspace\\"];
    for prefix in PREFIXES {
        if lower.starts_with(prefix) {
            return Some(s[prefix.len()..].trim_start_matches(['/', '\\']));
        }
    }
    if lower == "workspace" {
        return Some("");
    }
    if raw.starts_with("/workspace") {
        return Some(raw["/workspace".len()..].trim_start_matches(['/', '\\']));
    }
    if raw.starts_with("\\workspace") {
        return Some(raw["\\workspace".len()..].trim_start_matches(['/', '\\']));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_workspace_alias_to_root() {
        let ws = PathBuf::from(r"D:\proj");
        let p = resolve_tool_path(&[ws.clone()], "/workspace/moon-animation");
        assert_eq!(p, ws.join("moon-animation"));
    }

    #[test]
    fn maps_workspace_alias_without_leading_slash() {
        let ws = PathBuf::from(r"D:\proj");
        let p = resolve_tool_path(&[ws.clone()], "workspace/moon-animation");
        assert_eq!(p, ws.join("moon-animation"));
    }

    #[test]
    fn relative_paths_join_workspace() {
        let ws = PathBuf::from(r"D:\proj");
        assert_eq!(resolve_tool_path(&[ws.clone()], "src/main.rs"), ws.join("src/main.rs"));
    }
}
