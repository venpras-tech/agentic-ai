import { describe, expect, it } from "vitest";
import { highlight, languageFor } from "./highlight";

describe("languageFor", () => {
  it("maps common extensions", () => {
    expect(languageFor("src/app.ts")).toBe("ts");
    expect(languageFor("src/page.tsx")).toBe("ts");
    expect(languageFor("notes.json")).toBe("ts");
    expect(languageFor("main.rs")).toBe("rust");
    expect(languageFor("tool.py")).toBe("python");
    expect(languageFor("run.sh")).toBe("shell");
    expect(languageFor("index.html")).toBe("markup");
    expect(languageFor("style.css")).toBe("markup");
  });

  it("returns empty for unknown extensions", () => {
    expect(languageFor(".env")).toBe("");
    expect(languageFor("Makefile")).toBe("");
  });
});

describe("highlight", () => {
  it("escapes markup for plain text", () => {
    expect(highlight('const x = "<div>&"', "")).toBe('const x = "&lt;div&gt;&amp;"');
  });

  it("colors ts keywords, numbers, comments and strings", () => {
    const out = highlight('const x = 42; // note\nconst s = "hi";', "ts");
    expect(out).toContain('<span class="tok-kw">const</span>');
    expect(out).toContain('<span class="tok-num">42</span>');
    expect(out).toContain('<span class="tok-com">// note</span>');
    expect(out).toContain('<span class="tok-str">"hi"</span>');
  });

  it("colors function-call names", () => {
    const out = highlight("const y = findIt(x)", "ts");
    expect(out).toContain('<span class="tok-fn">findIt</span>');
  });

  it("colors rust keywords", () => {
    const out = highlight("fn main() { let mut n = 0; }", "rust");
    expect(out).toContain('<span class="tok-kw">fn</span>');
    expect(out).toContain('<span class="tok-kw">let</span>');
    expect(out).toContain('<span class="tok-kw">mut</span>');
  });

  it("colors python keywords and comments", () => {
    const out = highlight("def f():\n    return 1  # done", "python");
    expect(out).toContain('<span class="tok-kw">def</span>');
    expect(out).toContain('<span class="tok-kw">return</span>');
    expect(out).toContain('<span class="tok-com"># done</span>');
  });

  it("colors html tags", () => {
    const out = highlight("<div class=\"x\">text</div>", "markup");
    expect(out).toContain('<span class="tok-tag">&lt;div class="x"&gt;</span>');
  });

  it("keeps plain words uncolored", () => {
    const out = highlight("hello world foo", "ts");
    expect(out).not.toContain("tok-");
    expect(out).toBe("hello world foo");
  });

  it("handles escaped quotes inside strings", () => {
    const out = highlight('const s = "a\\"b";', "ts");
    expect(out).toContain('<span class="tok-str">"a\\"b"</span>');
  });
});