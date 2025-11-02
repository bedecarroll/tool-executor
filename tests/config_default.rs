use color_eyre::Result;
use tool_executor::config::model::SearchMode;
use tool_executor::config::{Config, default_template};

#[test]
fn default_config_template_parses() -> Result<()> {
    let template = default_template();
    let value: toml::Value = toml::from_str(template)?;
    let config = Config::from_value(&value)?;

    assert_eq!(config.defaults.provider.as_deref(), Some("codex"));
    assert_eq!(config.defaults.search_mode, SearchMode::FirstPrompt);

    Ok(())
}
