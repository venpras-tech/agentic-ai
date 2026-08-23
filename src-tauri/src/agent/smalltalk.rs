//! Small-talk detection shared by plain chat (`stream_inference`) and the
//! agent loop (`orchestrator`).
//!
//! History: both call sites used a "≤ 2 words && ≤ 20 chars" catch-all, which
//! hijacked real short queries like `2+3`, `5+7`, or `Calculate 5+7` and
//! answered them with a canned greeting. Detection is now vocabulary-gated:
//! short inputs only count as small talk when they START with an actual
//! greeting / thanks / farewell word (or exactly match a known phrase).

/// Words whose presence at the start of a short message signals small talk.
const LEAD_WORDS: &[&str] = &[
    "hi",
    "hello",
    "hey",
    "howdy",
    "sup",
    "yo",
    "hiya",
    "greetings",
    "thanks",
    "thank",
    "thx",
    "ty",
    "cheers",
    "bye",
    "goodbye",
    "later",
];

/// Exact multi-word phrases that are always small talk.
const PHRASES: &[&str] = &[
    "see you",
    "see ya",
    "good night",
    "good morning",
    "good afternoon",
    "good evening",
    "how are you",
    "how r u",
    "how's it going",
    "hows it going",
    "what's up",
    "whats up",
    "wsg",
    "thank you",
];

fn normalize(input: &str) -> &str {
    input
        .trim()
        .trim_end_matches(['!', '.', '?', ',', ';', ':'])
}

/// Returns `true` when `input` is trivial small talk that should get a canned
/// conversational reply instead of burning generation tokens.
pub fn is_smalltalk(input: &str) -> bool {
    let bare = normalize(input);
    if bare.is_empty() {
        return false;
    }
    let lower = bare.to_ascii_lowercase();
    if PHRASES.contains(&lower.as_str()) {
        return true;
    }
    // Short single-clause inputs led by a small-talk word. The length cap
    // keeps "hello, can you fix the parser in main.rs" out of the shortcut.
    let words: Vec<&str> = lower.split_whitespace().collect();
    words.len() <= 3 && lower.len() <= 24 && words.first().is_some_and(|w| LEAD_WORDS.contains(w))
}

/// Picks a short friendly reply for detected small talk.
pub fn reply_for(input: &str) -> String {
    let lower = normalize(input).to_ascii_lowercase();
    if lower.starts_with("bye")
        || (PHRASES.contains(&lower.as_str()) && lower.starts_with("good night"))
    {
        "Goodbye! Feel free to come back anytime.".to_string()
    } else if lower.starts_with("thank")
        || lower == "ty"
        || lower == "thx"
        || lower == "cheers"
        || lower.starts_with("thanks")
    {
        "You're welcome! Let me know if you need anything else.".to_string()
    } else if lower.starts_with("how are")
        || lower == "sup"
        || PHRASES.contains(&lower.as_str()) && lower.starts_with("what")
    {
        "I'm doing great, thanks! Ready to help with your code. What would you like to work on?"
            .to_string()
    } else {
        "Hi there! I'm your AI coding assistant. I can help you explore, edit, test, and fix your codebase. What would you like to work on?".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greetings_match() {
        for s in ["Hello", "hi!", "Hey there", "how are you", "Thanks!"] {
            assert!(is_smalltalk(s), "`{s}` should be small talk");
        }
    }

    #[test]
    fn math_queries_are_not_smalltalk() {
        for s in [
            "What is 2+3?",
            "Calculate 5+7",
            "2+3",
            "5*7+1",
            "(1+2)*3",
            "run the tests",
            "fix this bug",
        ] {
            assert!(!is_smalltalk(s), "`{s}` must reach the model/tools");
        }
    }

    #[test]
    fn long_messages_are_not_smalltalk_even_when_polite() {
        assert!(!is_smalltalk(
            "Hello, can you fix the parser in main.rs for me?"
        ));
    }

    #[test]
    fn replies_branch_by_intent() {
        assert!(reply_for("bye").starts_with("Goodbye"));
        assert!(reply_for("thank you").starts_with("You're welcome"));
        assert!(reply_for("hello").starts_with("Hi there"));
    }
}
