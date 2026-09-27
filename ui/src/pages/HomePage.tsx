import ChatComposer from "../components/ChatComposer";
import "./HomePage.css";

const suggestions = [
  "Add a login form to the app",
  "Find where authentication is handled",
  "Run the test suite and fix any failures",
];

function HomePage() {
  const handleSuggestionClick = (prompt: string) => {
    window.dispatchEvent(new CustomEvent("vencoder-prefill-composer", { detail: prompt }));
  };

  return (
    <div className="home-view">
      <div className="home-view-top">
        <h1 className="home-title">
          <span className="sidebar-logo-icon">AI</span>
          AI Editor
        </h1>
        <p className="home-desc">How can I help you today?</p>
        <div className="home-suggestions">
          {suggestions.map((suggestion) => (
            <button
              key={suggestion}
              type="button"
              className="chat-suggestion"
              onClick={() => handleSuggestionClick(suggestion)}
            >
              {suggestion}
            </button>
          ))}
        </div>
      </div>
      <div className="home-composer-wrap">
        <ChatComposer variant="home" />
      </div>
    </div>
  );
}

export default HomePage;