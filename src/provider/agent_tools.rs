use crate::config::config::AppConfig;
use serde_json::{Value, json};

pub const DEFAULT_SERVER_TOOLS_MODEL: &str = "grok-4-1-fast-reasoning";

pub fn supports_built_in_tools(model: &str) -> bool {
    model.starts_with("grok-4")
}

pub fn requested_built_in_tools_json(cfg: &AppConfig) -> Vec<Value> {
    let mut tools = Vec::new();

    if cfg.xai_web_search {
        tools.push(json!({ "type": "web_search" }));
    }
    if cfg.xai_x_search {
        tools.push(json!({ "type": "x_search" }));
    }
    if cfg.xai_code_interpreter {
        tools.push(json!({ "type": "code_interpreter" }));
    }

    tools
}

pub fn effective_built_in_tools_json(cfg: &AppConfig, model: &str) -> Vec<Value> {
    if supports_built_in_tools(model) {
        requested_built_in_tools_json(cfg)
    } else {
        Vec::new()
    }
}

pub fn effective_model_for_tools(cfg: &AppConfig, model: &str, api_mode: &str) -> String {
    let requested_tools = requested_built_in_tools_json(cfg);
    if !api_mode.eq_ignore_ascii_case("responses") || requested_tools.is_empty() {
        return model.to_string();
    }
    if supports_built_in_tools(model) {
        return model.to_string();
    }
    DEFAULT_SERVER_TOOLS_MODEL.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_tools_follow_config_flags() {
        let mut cfg = AppConfig::default();
        let defaults = requested_built_in_tools_json(&cfg);
        assert_eq!(
            defaults,
            vec![json!({ "type": "web_search" }), json!({ "type": "x_search" })]
        );

        cfg.xai_web_search = false;
        cfg.xai_x_search = false;
        cfg.xai_code_interpreter = true;
        let tools = requested_built_in_tools_json(&cfg);
        assert_eq!(tools, vec![json!({ "type": "code_interpreter" })]);
    }

    #[test]
    fn effective_built_in_tools_respect_model_family() {
        let cfg = AppConfig::default();
        assert!(effective_built_in_tools_json(&cfg, "grok-code-fast-1").is_empty());
        assert!(effective_built_in_tools_json(&cfg, "grok-4-1-fast-reasoning").len() >= 2);
    }

    #[test]
    fn unsupported_models_disable_built_in_tools() {
        assert!(supports_built_in_tools("grok-4-1-fast-reasoning"));
        assert!(supports_built_in_tools("grok-4.20-beta-0309-non-reasoning"));
        assert!(!supports_built_in_tools("grok-code-fast-1"));
    }

    #[test]
    fn effective_model_promotes_unsupported_tool_model() {
        let cfg = AppConfig::default();
        assert_eq!(
            effective_model_for_tools(&cfg, "grok-code-fast-1", "responses"),
            DEFAULT_SERVER_TOOLS_MODEL
        );
        assert_eq!(
            effective_model_for_tools(&cfg, "grok-4-1-fast-reasoning", "responses"),
            "grok-4-1-fast-reasoning"
        );
        assert_eq!(
            effective_model_for_tools(&cfg, "grok-code-fast-1", "chat_completions"),
            "grok-code-fast-1"
        );
    }
}
