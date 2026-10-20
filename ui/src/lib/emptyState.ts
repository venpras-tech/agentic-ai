import type { ActiveFileBuffer } from "../context/types";

export interface EmptySuggestion {
  label: string;
  prompt: string;
}

function baseName(path: string): string {
  const norm = path.replaceAll("\\", "/");
  return norm.slice(norm.lastIndexOf("/") + 1);
}

/**
 * Workspace-aware welcome suggestions (P5.5): when a file is open in the
 * editor, offer actions scoped to it (explain, test, review); otherwise fall
 * back to generic build/explore/run prompts. Keeps the keybinding hints in the
 * caller untouched.
 */
export function workspaceSuggestions(activeFile: ActiveFileBuffer | null): EmptySuggestion[] {
  const out: EmptySuggestion[] = [];
  if (activeFile?.path) {
    const name = baseName(activeFile.path);
    out.push(
      { label: `Explain ${name}`, prompt: `Explain what ${activeFile.path} does and how it fits the project.` },
      { label: `Review ${name}`, prompt: `Review ${activeFile.path} for bugs, edge cases, and clarity. Suggest concrete fixes.` },
      { label: `Add a test for ${name}`, prompt: `Add a test covering the main behaviour of ${activeFile.path}.` },
      { label: `Refactor ${name}`, prompt: `Refactor ${activeFile.path} for readability without changing behaviour.` },
    );
  }
  out.push(
    { label: "Find auth logic", prompt: "Find where authentication is handled" },
    { label: "Run tests & fix", prompt: "Run the test suite and fix any failures" },
    { label: "Add a login form", prompt: "Add a login form to the app" },
  );
  return out.slice(0, 6);
}
