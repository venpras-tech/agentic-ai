import "./HelpPage.css";

function HelpPage() {
  return (
    <div className="help-view">
      <h2 className="help-title">Vencoder Help</h2>

      <section className="help-section">
        <h3 className="help-section-title">Modes</h3>
        <p className="help-desc">Choose how the AI responds. Use the dropdown above the input.</p>
        <dl className="help-dl">
          <dt>Agent</dt>
          <dd>Default mode. Defines the task, clarifies when ambiguous, then implements. The AI asks 1–3 specific questions if the request is vague before coding. Uses file structure context to avoid hallucinations. Best for well-defined tasks.</dd>
          <dt>Ask</dt>
          <dd>Read-only mode. Explores and answers without changes. Asks focused follow-ups when questions are vague. Use for learning, planning, and understanding code before modifying it.</dd>
          <dt>Plan</dt>
          <dd>Creates a detailed plan before any code. Researches the codebase, asks clarifying questions, and produces a reviewable plan with file paths and numbered steps. Use for complex, multi-file changes. <kbd>Shift+Tab</kbd> to switch quickly.</dd>
        </dl>
      </section>

      <section className="help-section">
        <h3 className="help-section-title">Slash Commands</h3>
        <p className="help-desc">Type in the chat input and press Enter:</p>
        <dl className="help-dl">
          <dt><kbd>/new</kbd> or <kbd>/n</kbd></dt>
          <dd>Start a new chat</dd>
          <dt><kbd>/models</kbd> or <kbd>/m</kbd></dt>
          <dd>Open Models page</dd>
          <dt><kbd>/index</kbd> or <kbd>/i</kbd></dt>
          <dd>Open Workspace page</dd>
          <dt><kbd>/clear</kbd></dt>
          <dd>Clear context chips</dd>
          <dt><kbd>/mode agent|ask|plan</kbd></dt>
          <dd>Switch agent mode</dd>
        </dl>
      </section>

      <section className="help-section">
        <h3 className="help-section-title">Context</h3>
        <p className="help-desc">Use the context buttons below the input to add context. Context appears as chips above the buttons.</p>
        <dl className="help-dl">
          <dt>@Files</dt>
          <dd>Include full file contents. Right-click a file in the Workspace explorer to add it.</dd>
          <dt>@Codebase</dt>
          <dd>Semantic search over the workspace. The AI gets relevant chunks based on your message.</dd>
          <dt>@Git</dt>
          <dd>Include git log or diff.</dd>
          <dt>@Past Chats</dt>
          <dd>Include recent messages from the current conversation as context.</dd>
        </dl>
      </section>

      <section className="help-section">
        <h3 className="help-section-title">Keyboard Shortcuts</h3>
        <dl className="help-dl">
          <dt><kbd>Enter</kbd></dt>
          <dd>Send message</dd>
          <dt><kbd>Shift+Enter</kbd></dt>
          <dd>New line in input</dd>
          <dt><kbd>Ctrl+N</kbd></dt>
          <dd>New chat</dd>
          <dt><kbd>Ctrl+K</kbd></dt>
          <dd>Focus chat input</dd>
          <dt><kbd>Ctrl+Shift+P</kbd></dt>
          <dd>Open the command palette (navigate pages, new chat, toggle theme…)</dd>
          <dt><kbd>Ctrl+Shift+H</kbd></dt>
          <dd>Toggle chat history panel</dd>
          <dt><kbd>Ctrl+1</kbd>–<kbd>5</kbd></dt>
          <dd>Switch pages (Home, Chat, Project, Models, Settings)</dd>
          <dt><kbd>Shift+Tab</kbd></dt>
          <dd>Switch to Plan mode</dd>
          <dt><kbd>Escape</kbd></dt>
          <dd>Cancel generation if in progress; otherwise close the chat history side panel</dd>
          <dt><kbd>Ctrl+↑</kbd> / <kbd>Ctrl+↓</kbd></dt>
          <dd>Navigate input history (previous/next user message)</dd>
        </dl>
      </section>
    </div>
  );
}

export default HelpPage;