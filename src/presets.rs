//! Provider presets: one name stands in for a provider's base URL, so a
//! harness can point at `dashcam record --provider X` without hunting the
//! URL down. `--upstream` still wins when both are given, so a mirror or a
//! gateway keeps working.

/// A known provider's base URL. WebSocket bases are derived by swapping the
/// scheme (`https` -> `wss`, `http` -> `ws`), so realtime agents get the same
/// treatment without a second table.
pub struct ProviderPreset {
    pub name: &'static str,
    pub base: &'static str,
}

const PRESETS: &[ProviderPreset] = &[
    ProviderPreset { name: "openai", base: "https://api.openai.com" },
    ProviderPreset { name: "anthropic", base: "https://api.anthropic.com" },
    ProviderPreset { name: "google", base: "https://generativelanguage.googleapis.com" },
    ProviderPreset { name: "gemini", base: "https://generativelanguage.googleapis.com" },
    ProviderPreset { name: "nim", base: "https://integrate.api.nvidia.com" },
    ProviderPreset { name: "nvidia", base: "https://integrate.api.nvidia.com" },
    ProviderPreset { name: "openrouter", base: "https://openrouter.ai" },
    ProviderPreset { name: "groq", base: "https://api.groq.com" },
    ProviderPreset { name: "deepseek", base: "https://api.deepseek.com" },
    ProviderPreset { name: "mistral", base: "https://api.mistral.ai" },
    ProviderPreset { name: "xai", base: "https://api.x.ai" },
    ProviderPreset { name: "together", base: "https://api.together.xyz" },
    ProviderPreset { name: "fireworks", base: "https://api.fireworks.ai" },
    ProviderPreset { name: "cerebras", base: "https://api.cerebras.ai" },
    ProviderPreset { name: "perplexity", base: "https://api.perplexity.ai" },
    ProviderPreset { name: "ollama", base: "http://127.0.0.1:11434" },
    ProviderPreset { name: "lmstudio", base: "http://127.0.0.1:1234" },
];

/// The base URL a preset name stands for, matched case-insensitively.
pub fn lookup(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    PRESETS.iter().find(|p| p.name == name).map(|p| p.base)
}

/// Swap the scheme of a base URL to its WebSocket twin.
pub fn ws_of(base: &str) -> String {
    if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    }
}

/// Resolve the effective upstream: explicit `--upstream` wins, then the
/// provider preset, else an error naming what to pass.
pub fn resolve(upstream: Option<&str>, provider: Option<&str>) -> Result<String, String> {
    if let Some(u) = upstream {
        return Ok(u.to_string());
    }
    match provider {
        Some(p) => lookup(p)
            .map(str::to_string)
            .ok_or_else(|| unknown_provider(p)),
        None => Err("no provider given: pass --provider NAME (openai, anthropic, google, nim, openrouter, ollama, ...) or --upstream URL".to_string()),
    }
}

fn unknown_provider(name: &str) -> String {
    let mut names: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
    names.sort_unstable();
    names.dedup();
    format!("unknown provider \"{name}\"; known providers: {}", names.join(", "))
}

/// Env pairs pointing the common harness base-URL variables at the recorder.
/// Most harnesses read one of these and hand it to every agent they spawn,
/// swarms included, so one line captures the whole tree.
pub fn env_pairs(port: u16, upstream: &str) -> Vec<(String, String)> {
    let local = format!("http://127.0.0.1:{port}");
    vec![
        ("OPENAI_BASE_URL".to_string(), format!("{local}/v1")),
        ("OPENAI_API_BASE".to_string(), format!("{local}/v1")),
        ("ANTHROPIC_BASE_URL".to_string(), local),
        ("DASHCAM_UPSTREAM".to_string(), upstream.to_string()),
    ]
}

/// The same pairs as POSIX `export` lines for `eval "$(dashcam env)"`.
pub fn env_lines(port: u16, upstream: &str) -> Vec<String> {
    env_pairs(port, upstream)
        .into_iter()
        .map(|(k, v)| format!("export {k}=\"{v}\""))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_matches_known_names_and_aliases() {
        assert_eq!(lookup("openai"), Some("https://api.openai.com"));
        assert_eq!(lookup("OPENAI"), Some("https://api.openai.com"));
        assert_eq!(lookup("gemini"), lookup("google"));
        assert_eq!(lookup("nvidia"), lookup("nim"));
        assert_eq!(lookup("nope"), None);
    }

    #[test]
    fn ws_of_swaps_schemes() {
        assert_eq!(ws_of("https://api.openai.com"), "wss://api.openai.com");
        assert_eq!(ws_of("http://127.0.0.1:11434"), "ws://127.0.0.1:11434");
        assert_eq!(ws_of("unix-ish"), "unix-ish");
    }

    #[test]
    fn resolve_prefers_explicit_upstream() {
        let u = resolve(Some("https://my-gateway.example"), Some("openai")).unwrap();
        assert_eq!(u, "https://my-gateway.example");
    }

    #[test]
    fn resolve_uses_preset_when_only_provider() {
        let u = resolve(None, Some("anthropic")).unwrap();
        assert_eq!(u, "https://api.anthropic.com");
    }

    #[test]
    fn resolve_errors_without_either() {
        assert!(resolve(None, None).is_err());
    }

    #[test]
    fn resolve_errors_on_unknown_provider_and_lists_known() {
        let err = resolve(None, Some("nope")).unwrap_err();
        assert!(err.contains("unknown provider \"nope\""));
        assert!(err.contains("openai"));
        assert!(err.contains("ollama"));
    }

    #[test]
    fn env_pairs_shape_the_local_bases() {
        let pairs = env_pairs(8099, "https://api.openai.com");
        let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()).unwrap();
        assert_eq!(get("OPENAI_BASE_URL"), "http://127.0.0.1:8099/v1");
        assert_eq!(get("OPENAI_API_BASE"), "http://127.0.0.1:8099/v1");
        assert_eq!(get("ANTHROPIC_BASE_URL"), "http://127.0.0.1:8099");
        assert_eq!(get("DASHCAM_UPSTREAM"), "https://api.openai.com");
    }

    #[test]
    fn env_lines_are_posix_exports() {
        let lines = env_lines(8099, "https://api.openai.com");
        assert!(lines.iter().all(|l| l.starts_with("export ")));
        assert!(lines.iter().any(|l| l.contains("OPENAI_BASE_URL=\"http://127.0.0.1:8099/v1\"")));
    }
}
