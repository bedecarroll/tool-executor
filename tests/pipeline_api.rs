use std::collections::HashMap;
use std::path::PathBuf;

use indexmap::IndexMap;
use tool_executor::config::model::{
    Config, Defaults, FeatureConfig, ProviderConfig, SearchMode, Snippet, SnippetConfig,
    StdinMapping, StdinMode,
};
use tool_executor::pipeline::{PipelineRequest, PromptInvocation, SessionContext, build_pipeline};

fn sample_config() -> Config {
    let mut providers = IndexMap::new();
    providers.insert(
        "codex".to_string(),
        ProviderConfig {
            name: "codex".to_string(),
            bin: "codex".to_string(),
            flags: vec!["--search".to_string()],
            env: Vec::new(),
            session_roots: Vec::new(),
            stdin: Some(StdinMapping {
                args: vec!["{prompt}".to_string()],
                mode: StdinMode::CaptureArg,
            }),
        },
    );

    let mut pre = IndexMap::new();
    pre.insert(
        "prep".to_string(),
        Snippet {
            name: "prep".to_string(),
            command: "printf 'prep'".to_string(),
        },
    );

    Config {
        defaults: Defaults {
            provider: Some("codex".to_string()),
            profile: None,
            search_mode: SearchMode::FirstPrompt,
            terminal_title: Some("{{provider}} {{var:USER}}".to_string()),
        },
        providers,
        snippets: SnippetConfig {
            pre,
            post: IndexMap::new(),
        },
        wrappers: IndexMap::new(),
        profiles: IndexMap::new(),
        features: FeatureConfig {
            prompt_assembler: None,
        },
    }
}

#[test]
fn build_pipeline_supports_capture_arg_with_prompt_assembler() -> color_eyre::Result<()> {
    let config = sample_config();
    let mut vars = HashMap::new();
    vars.insert("USER".to_string(), "demo".to_string());

    let request = PipelineRequest {
        config: &config,
        provider_hint: Some("codex"),
        profile: None,
        additional_pre: vec!["prep".to_string()],
        additional_post: Vec::new(),
        inline_pre: vec!["printf 'inline'".to_string()],
        wrap: None,
        provider_args: vec!["--mode".to_string(), "chat".to_string()],
        capture_prompt: true,
        prompt_assembler: Some(PromptInvocation {
            name: "demo".to_string(),
            args: vec!["arg1".to_string()],
        }),
        vars,
        session: SessionContext {
            id: Some("sess-1".to_string()),
            label: Some("Session".to_string()),
            path: Some("/tmp/session.jsonl".to_string()),
            resume_token: Some("resume-token".to_string()),
        },
        cwd: PathBuf::from("/tmp"),
    };

    let plan = build_pipeline(&request)?;
    assert!(plan.uses_capture_arg);
    assert!(plan.capture_has_pre_commands);
    assert!(plan.pipeline.contains("internal capture-arg"));
    assert!(plan.display.contains("internal capture-arg"));
    assert_eq!(plan.terminal_title, "codex demo");
    Ok(())
}

#[test]
fn build_pipeline_errors_for_missing_wrapper() {
    let config = sample_config();
    let request = PipelineRequest {
        config: &config,
        provider_hint: Some("codex"),
        profile: None,
        additional_pre: Vec::new(),
        additional_post: Vec::new(),
        inline_pre: Vec::new(),
        wrap: Some("missing"),
        provider_args: Vec::new(),
        capture_prompt: false,
        prompt_assembler: None,
        vars: HashMap::new(),
        session: SessionContext::default(),
        cwd: PathBuf::from("."),
    };

    let err = build_pipeline(&request).expect_err("missing wrapper should fail");
    assert!(err.to_string().contains("wrapper 'missing' not found"));
}

#[test]
fn build_pipeline_errors_for_missing_profile() {
    let config = sample_config();
    let request = PipelineRequest {
        config: &config,
        provider_hint: None,
        profile: Some("missing"),
        additional_pre: Vec::new(),
        additional_post: Vec::new(),
        inline_pre: Vec::new(),
        wrap: None,
        provider_args: Vec::new(),
        capture_prompt: false,
        prompt_assembler: None,
        vars: HashMap::new(),
        session: SessionContext::default(),
        cwd: PathBuf::from("."),
    };

    let err = build_pipeline(&request).expect_err("missing profile should fail");
    assert!(err.to_string().contains("profile 'missing' not found"));
}

#[test]
fn build_pipeline_errors_when_no_provider_selected() {
    let mut config = sample_config();
    config.defaults.provider = None;
    let request = PipelineRequest {
        config: &config,
        provider_hint: None,
        profile: None,
        additional_pre: Vec::new(),
        additional_post: Vec::new(),
        inline_pre: Vec::new(),
        wrap: None,
        provider_args: Vec::new(),
        capture_prompt: false,
        prompt_assembler: None,
        vars: HashMap::new(),
        session: SessionContext::default(),
        cwd: PathBuf::from("."),
    };

    let err = build_pipeline(&request).expect_err("missing provider selection should fail");
    assert!(
        err.to_string()
            .contains("no provider selected; specify --profile or <provider>")
    );
}

#[test]
fn build_pipeline_errors_for_unknown_provider_hint() {
    let mut config = sample_config();
    config.defaults.provider = None;
    let request = PipelineRequest {
        config: &config,
        provider_hint: Some("unknown"),
        profile: None,
        additional_pre: Vec::new(),
        additional_post: Vec::new(),
        inline_pre: Vec::new(),
        wrap: None,
        provider_args: Vec::new(),
        capture_prompt: false,
        prompt_assembler: None,
        vars: HashMap::new(),
        session: SessionContext::default(),
        cwd: PathBuf::from("."),
    };

    let err = build_pipeline(&request).expect_err("unknown provider should fail");
    assert!(err.to_string().contains("provider 'unknown' not defined"));
}
