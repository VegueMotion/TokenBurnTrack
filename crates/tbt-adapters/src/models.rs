pub fn known_provider(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") {
        Some("anthropic")
    } else if m.starts_with("gpt")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
    {
        Some("openai")
    } else if m.starts_with("gemini") {
        Some("google")
    } else if m.starts_with("grok") {
        Some("xai")
    } else if m.starts_with("deepseek") {
        Some("deepseek")
    } else {
        None
    }
}
