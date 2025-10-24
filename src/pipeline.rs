use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use regex::Regex;
use shell_escape::unix::escape as shell_escape;
use std::sync::LazyLock;

use crate::config::model::{
    Config, ProfileConfig, ProviderConfig, Snippet, StdinMode, WrapperConfig, WrapperMode,
};

#[derive(Debug, Clone)]
pub struct PipelineRequest<'a> {
    pub config: &'a Config,
    pub provider_hint: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub additional_pre: Vec<String>,
    pub additional_post: Vec<String>,
    pub inline_pre: Vec<String>,
    pub wrap: Option<&'a str>,
    pub provider_args: Vec<String>,
    pub capture_prompt: bool,
    pub vars: HashMap<String, String>,
    pub session: SessionContext,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct SessionContext {
    pub id: Option<String>,
    pub label: Option<String>,
    pub path: Option<String>,
    pub resume_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PipelinePlan {
    pub pipeline: String,
    pub display: String,
    pub friendly_display: String,
    pub env: Vec<(String, String)>,
    pub invocation: Invocation,
    pub provider: String,
    pub pre_snippets: Vec<String>,
    pub post_snippets: Vec<String>,
    pub wrapper: Option<String>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone)]
pub enum Invocation {
    Shell { command: String },
    Exec { argv: Vec<String> },
}

static ENV_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{env:([A-Za-z0-9_]+)\}").unwrap());
static TEMPLATE_TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\{([^}]+)\}\}").unwrap());

/// Construct a pipeline plan from the provided request and configuration.
///
/// # Errors
///
/// Returns an error when referenced profiles, snippets, or wrappers are missing or
/// when template rendering fails.
pub fn build_pipeline(request: &PipelineRequest<'_>) -> Result<PipelinePlan> {
    let config = request.config;
    let profile = resolve_profile(config, request.profile)?;
    let provider = resolve_provider(config, profile, request.provider_hint)?;
    let (wrapper, wrap_name) = determine_wrapper(config, request.wrap, profile)?;

    let (pre_snippet_names, post_snippet_names) = collect_snippet_names(profile, request);
    let pre_commands = build_pre_commands(config, &pre_snippet_names, &request.inline_pre)?;
    let post_commands = resolve_snippets(&config.snippets.post, &post_snippet_names, "post")?;

    let provider_capture = provider
        .stdin
        .as_ref()
        .is_some_and(|stdin| matches!(stdin.mode, StdinMode::CaptureArg));
    let provider_args = build_provider_args(provider, request, provider_capture);
    let capture_prompt = request.capture_prompt && provider_capture;

    let stages = compose_stages(
        provider,
        capture_prompt,
        &pre_commands,
        &post_commands,
        &provider_args,
    );
    let pipeline = stages.join(" | ");
    let env = render_env(provider)?;

    let cwd_str = request.cwd.to_string_lossy().to_string();
    let template_ctx = TemplateContext {
        pipeline: &pipeline,
        provider: &provider.name,
        session_id: request.session.id.as_deref(),
        session_label: request.session.label.as_deref(),
        session_path: request.session.path.as_deref(),
        session_resume_token: request.session.resume_token.as_deref(),
        cwd: &cwd_str,
        vars: &request.vars,
    };

    let invocation = match wrapper {
        Some(wrapper) => render_wrapper(wrapper, &template_ctx)?,
        None => Invocation::Shell {
            command: pipeline.clone(),
        },
    };

    let display = match &invocation {
        Invocation::Shell { command } => command.clone(),
        Invocation::Exec { argv } => argv
            .iter()
            .map(|arg| shell_escape(Cow::Borrowed(arg)).to_string())
            .collect::<Vec<_>>()
            .join(" "),
    };

    let friendly_display = if wrapper.is_none() && capture_prompt {
        friendly_capture_display(
            provider,
            &pre_commands,
            &post_commands,
            &provider_args,
            &display,
        )
    } else {
        display.clone()
    };

    Ok(PipelinePlan {
        pipeline,
        display,
        friendly_display,
        env,
        invocation,
        provider: provider.name.clone(),
        pre_snippets: pre_snippet_names,
        post_snippets: post_snippet_names,
        wrapper: wrap_name,
        cwd: request.cwd.clone(),
    })
}

fn resolve_profile<'a>(
    config: &'a Config,
    profile_name: Option<&str>,
) -> Result<Option<&'a ProfileConfig>> {
    match profile_name {
        Some(name) => config
            .profiles
            .get(name)
            .map(Some)
            .ok_or_else(|| eyre!("profile '{name}' not found")),
        None => Ok(None),
    }
}

fn resolve_provider<'a>(
    config: &'a Config,
    profile: Option<&ProfileConfig>,
    provider_hint: Option<&str>,
) -> Result<&'a ProviderConfig> {
    let provider_name = profile
        .map(|p| p.provider.as_str())
        .or(provider_hint)
        .or(config.defaults.provider.as_deref())
        .ok_or_else(|| eyre!("no provider selected; specify --profile or <provider>"))?;

    config
        .providers
        .get(provider_name)
        .ok_or_else(|| eyre!("provider '{provider_name}' not defined"))
}

fn determine_wrapper<'a>(
    config: &'a Config,
    requested_wrap: Option<&str>,
    profile: Option<&ProfileConfig>,
) -> Result<(Option<&'a WrapperConfig>, Option<String>)> {
    let wrap_name = requested_wrap
        .map(str::to_string)
        .or_else(|| profile.and_then(|p| p.wrap.clone()));

    let wrapper = match wrap_name.as_deref() {
        Some(name) => Some(
            config
                .wrappers
                .get(name)
                .ok_or_else(|| eyre!("wrapper '{name}' not found"))?,
        ),
        None => None,
    };

    Ok((wrapper, wrap_name))
}

fn collect_snippet_names(
    profile: Option<&ProfileConfig>,
    request: &PipelineRequest<'_>,
) -> (Vec<String>, Vec<String>) {
    let mut pre = profile.map(|p| p.pre.clone()).unwrap_or_default();
    pre.extend(request.additional_pre.iter().cloned());

    let mut post = profile.map(|p| p.post.clone()).unwrap_or_default();
    post.extend(request.additional_post.iter().cloned());

    (pre, post)
}

fn build_pre_commands(
    config: &Config,
    snippet_names: &[String],
    inline_commands: &[String],
) -> Result<Vec<String>> {
    let mut commands = resolve_snippets(&config.snippets.pre, snippet_names, "pre")?;
    commands.extend(inline_commands.iter().cloned());
    Ok(commands)
}

fn build_provider_args(
    provider: &ProviderConfig,
    request: &PipelineRequest<'_>,
    provider_capture: bool,
) -> Vec<String> {
    let mut provider_args = provider.flags.clone();
    if let Some(stdin) = &provider.stdin {
        if provider_capture {
            if request.capture_prompt {
                provider_args.extend(stdin.args.clone());
            }
        } else {
            provider_args.extend(stdin.args.clone());
        }
    }
    provider_args.extend(request.provider_args.iter().cloned());
    provider_args
}

fn compose_stages(
    provider: &ProviderConfig,
    capture_prompt: bool,
    pre_commands: &[String],
    post_commands: &[String],
    provider_args: &[String],
) -> Vec<String> {
    let mut stages = Vec::new();
    if capture_prompt {
        stages.push(build_capture_command(provider, pre_commands, provider_args));
    } else {
        stages.extend(pre_commands.iter().cloned());
        stages.push(command_string(&provider.bin, provider_args));
    }
    stages.extend(post_commands.iter().cloned());
    stages
}

#[derive(Debug)]
struct FriendlyArg {
    text: String,
    needs_escape: bool,
}

fn friendly_capture_display(
    provider: &ProviderConfig,
    pre_commands: &[String],
    post_commands: &[String],
    provider_args: &[String],
    fallback: &str,
) -> String {
    let substitution_pipeline = if pre_commands.is_empty() {
        None
    } else {
        Some(pre_commands.join(" | "))
    };

    let substitution_raw = substitution_pipeline
        .as_ref()
        .map(|pipeline| format!("$({pipeline})"));
    let substitution_quoted = substitution_pipeline.as_ref().map(|pipeline| {
        let escaped = pipeline.replace('"', "\\\"");
        format!("\"$({escaped})\"")
    });

    let mut args = Vec::new();

    for arg in provider_args {
        if arg == "{prompt}" {
            if let Some(quoted) = &substitution_quoted {
                args.push(FriendlyArg {
                    text: quoted.clone(),
                    needs_escape: false,
                });
            }
            continue;
        }

        if arg.contains("{prompt}") {
            if let Some(raw) = &substitution_raw {
                let replaced = arg.replace("{prompt}", raw);
                let needs_quotes = arg.contains(' ') || arg.contains('"');
                if needs_quotes {
                    let escaped = replaced.replace('"', "\\\"");
                    args.push(FriendlyArg {
                        text: format!("\"{escaped}\""),
                        needs_escape: false,
                    });
                } else {
                    args.push(FriendlyArg {
                        text: replaced,
                        needs_escape: false,
                    });
                }
            } else {
                let replaced = arg.replace("{prompt}", "<prompt>");
                args.push(FriendlyArg {
                    text: replaced,
                    needs_escape: true,
                });
            }
            continue;
        }

        args.push(FriendlyArg {
            text: arg.clone(),
            needs_escape: true,
        });
    }

    let provider_stage = assemble_friendly_command(&provider.bin, &args);
    let mut stages = Vec::with_capacity(post_commands.len() + 1);
    stages.push(provider_stage);
    stages.extend(post_commands.iter().cloned());

    if stages.is_empty() {
        fallback.to_string()
    } else {
        stages.join(" | ")
    }
}

fn assemble_friendly_command(bin: &str, args: &[FriendlyArg]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shell_escape(Cow::Borrowed(bin)).to_string());
    for arg in args {
        if arg.needs_escape {
            parts.push(shell_escape(Cow::Borrowed(arg.text.as_str())).to_string());
        } else {
            parts.push(arg.text.clone());
        }
    }
    parts.join(" ")
}

fn resolve_snippets(
    snippets: &indexmap::IndexMap<String, Snippet>,
    names: &[String],
    kind: &str,
) -> Result<Vec<String>> {
    let mut commands = Vec::new();
    for name in names {
        let snippet = snippets.get(name).ok_or_else(|| {
            eyre!("unknown {kind} snippet '{name}' — define it under [snippets.{kind}] in configuration")
        })?;
        commands.push(snippet.command.clone());
    }
    Ok(commands)
}

fn render_env(provider: &ProviderConfig) -> Result<Vec<(String, String)>> {
    provider
        .env
        .iter()
        .map(|entry| {
            let value = expand_env_template(&entry.value_template).with_context(|| {
                format!(
                    "while expanding ${} for provider {}",
                    entry.key, provider.name
                )
            })?;
            Ok((entry.key.clone(), value))
        })
        .collect()
}

fn build_capture_command(
    provider: &ProviderConfig,
    pre_commands: &[String],
    provider_args: &[String],
) -> String {
    let mut internal_args = vec![
        "internal".to_string(),
        "capture-arg".to_string(),
        "--provider".to_string(),
        provider.name.clone(),
        "--bin".to_string(),
        provider.bin.clone(),
    ];
    for pre in pre_commands {
        internal_args.push("--pre".to_string());
        internal_args.push(pre.clone());
    }
    for arg in provider_args {
        internal_args.push("--arg".to_string());
        internal_args.push(arg.clone());
    }

    let tx_path = std::env::current_exe()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .unwrap_or_else(|| "tx".to_string());
    command_string(&tx_path, &internal_args)
}

fn expand_env_template(template: &str) -> Result<String> {
    let mut result = String::new();
    let mut last = 0;
    for caps in ENV_TOKEN.captures_iter(template) {
        let mat = caps.get(0).expect("match");
        result.push_str(&template[last..mat.start()]);
        let var = caps.get(1).unwrap().as_str();
        let replacement =
            std::env::var(var).map_err(|_| eyre!("environment variable '{var}' not set"))?;
        result.push_str(&replacement);
        last = mat.end();
    }
    result.push_str(&template[last..]);
    Ok(result)
}

#[derive(Debug)]
struct TemplateContext<'a> {
    pipeline: &'a str,
    provider: &'a str,
    session_id: Option<&'a str>,
    session_label: Option<&'a str>,
    session_path: Option<&'a str>,
    session_resume_token: Option<&'a str>,
    cwd: &'a str,
    vars: &'a HashMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
enum CmdMode {
    Raw,
    Shell,
}

fn render_wrapper(wrapper: &WrapperConfig, ctx: &TemplateContext<'_>) -> Result<Invocation> {
    match &wrapper.mode {
        WrapperMode::Shell { command } => {
            let rendered = render_template(command, ctx, CmdMode::Shell)?;
            Ok(Invocation::Shell { command: rendered })
        }
        WrapperMode::Exec { argv } => {
            let rendered = argv
                .iter()
                .map(|arg| render_template(arg, ctx, CmdMode::Raw))
                .collect::<Result<Vec<_>>>()?;
            Ok(Invocation::Exec { argv: rendered })
        }
    }
}

fn render_template(input: &str, ctx: &TemplateContext<'_>, mode: CmdMode) -> Result<String> {
    let mut out = String::new();
    let mut last = 0;
    for caps in TEMPLATE_TOKEN.captures_iter(input) {
        let mat = caps.get(0).unwrap();
        out.push_str(&input[last..mat.start()]);
        let key = caps.get(1).unwrap().as_str();
        let replacement = resolve_placeholder(key, ctx, mode)?;
        out.push_str(&replacement);
        last = mat.end();
    }
    out.push_str(&input[last..]);
    Ok(out)
}

fn resolve_placeholder(key: &str, ctx: &TemplateContext<'_>, mode: CmdMode) -> Result<String> {
    match key {
        "CMD" => Ok(match mode {
            CmdMode::Raw => ctx.pipeline.to_string(),
            CmdMode::Shell => single_quote(ctx.pipeline),
        }),
        "provider" => Ok(ctx.provider.to_string()),
        "session.id" => Ok(ctx.session_id.unwrap_or("").to_string()),
        "session.label" => Ok(ctx.session_label.unwrap_or("").to_string()),
        "session.path" => Ok(ctx.session_path.unwrap_or("").to_string()),
        "session.resume_token" => Ok(ctx.session_resume_token.unwrap_or("").to_string()),
        "cwd" => Ok(ctx.cwd.to_string()),
        other if other.starts_with("var:") => {
            let name = &other[4..];
            let value = ctx
                .vars
                .get(name)
                .ok_or_else(|| eyre!("missing value for variable '{name}'"))?;
            Ok(value.clone())
        }
        other => Err(eyre!("unknown template placeholder '{{{{{other}}}}}'")),
    }
}

fn single_quote(input: &str) -> String {
    let mut quoted = String::with_capacity(input.len() + 2);
    quoted.push('\'');
    for ch in input.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

fn command_string(bin: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shell_escape(Cow::Borrowed(bin)).to_string());
    for arg in args {
        parts.push(shell_escape(Cow::Borrowed(arg)).to_string());
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{
        Defaults, FeatureConfig, ProviderConfig, SearchMode, SnippetConfig, StdinMapping,
    };
    use indexmap::IndexMap;
    use std::collections::HashMap;

    #[test]
    fn inline_pre_commands_are_included_for_capture_arg() {
        let mut providers = IndexMap::new();
        providers.insert(
            "codex".into(),
            ProviderConfig {
                name: "codex".into(),
                bin: "codex".into(),
                flags: vec!["--search".into()],
                env: Vec::new(),
                session_roots: Vec::new(),
                stdin: Some(StdinMapping {
                    args: vec!["{prompt}".into()],
                    mode: StdinMode::CaptureArg,
                }),
            },
        );

        let config = Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: None,
                search_mode: SearchMode::FirstPrompt,
                preview_filter: None,
            },
            providers,
            snippets: SnippetConfig {
                pre: IndexMap::new(),
                post: IndexMap::new(),
            },
            wrappers: IndexMap::new(),
            profiles: IndexMap::new(),
            features: FeatureConfig {
                prompt_assembler: None,
            },
        };

        let request = PipelineRequest {
            config: &config,
            provider_hint: Some("codex"),
            profile: None,
            additional_pre: Vec::new(),
            additional_post: Vec::new(),
            inline_pre: vec!["pa hello".into()],
            wrap: None,
            provider_args: Vec::new(),
            capture_prompt: true,
            vars: HashMap::new(),
            session: SessionContext::default(),
            cwd: PathBuf::from("/tmp"),
        };

        let plan = build_pipeline(&request).expect("pipeline builds");
        assert!(
            plan.pipeline.contains("pa hello"),
            "expected inline pre command in pipeline: {}",
            plan.pipeline
        );
        assert!(
            plan.pipeline.contains("{prompt}"),
            "expected captured prompt placeholder in pipeline: {}",
            plan.pipeline
        );
        assert_eq!(
            plan.friendly_display, r#"codex --search "$(pa hello)""#,
            "expected friendly display to use command substitution"
        );
    }

    #[test]
    fn capture_arg_is_skipped_when_disabled() {
        let mut providers = IndexMap::new();
        providers.insert(
            "codex".into(),
            ProviderConfig {
                name: "codex".into(),
                bin: "codex".into(),
                flags: vec!["--search".into()],
                env: Vec::new(),
                session_roots: Vec::new(),
                stdin: Some(StdinMapping {
                    args: vec!["{prompt}".into()],
                    mode: StdinMode::CaptureArg,
                }),
            },
        );

        let config = Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: None,
                search_mode: SearchMode::FirstPrompt,
                preview_filter: None,
            },
            providers,
            snippets: SnippetConfig {
                pre: IndexMap::new(),
                post: IndexMap::new(),
            },
            wrappers: IndexMap::new(),
            profiles: IndexMap::new(),
            features: FeatureConfig {
                prompt_assembler: None,
            },
        };

        let request = PipelineRequest {
            config: &config,
            provider_hint: Some("codex"),
            profile: None,
            additional_pre: Vec::new(),
            additional_post: Vec::new(),
            inline_pre: Vec::new(),
            wrap: None,
            provider_args: Vec::new(),
            capture_prompt: false,
            vars: HashMap::new(),
            session: SessionContext::default(),
            cwd: PathBuf::from("/tmp"),
        };

        let plan = build_pipeline(&request).expect("pipeline builds");
        assert!(
            !plan.pipeline.contains("internal capture-arg"),
            "expected capture helper to be skipped: {}",
            plan.pipeline
        );
        assert!(
            plan.pipeline.contains("codex --search"),
            "expected provider invocation in pipeline: {}",
            plan.pipeline
        );
        assert!(
            !plan.pipeline.contains("{prompt}"),
            "expected prompt placeholder to be absent: {}",
            plan.pipeline
        );
        assert_eq!(
            plan.friendly_display, plan.display,
            "expected friendly display to match pipeline when capture is disabled"
        );
    }

    #[test]
    fn friendly_display_for_capture_without_pre() {
        let mut providers = IndexMap::new();
        providers.insert(
            "codex".into(),
            ProviderConfig {
                name: "codex".into(),
                bin: "codex".into(),
                flags: vec!["--search".into()],
                env: Vec::new(),
                session_roots: Vec::new(),
                stdin: Some(StdinMapping {
                    args: vec!["{prompt}".into()],
                    mode: StdinMode::CaptureArg,
                }),
            },
        );

        let config = Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: None,
                search_mode: SearchMode::FirstPrompt,
                preview_filter: None,
            },
            providers,
            snippets: SnippetConfig {
                pre: IndexMap::new(),
                post: IndexMap::new(),
            },
            wrappers: IndexMap::new(),
            profiles: IndexMap::new(),
            features: FeatureConfig {
                prompt_assembler: None,
            },
        };

        let request = PipelineRequest {
            config: &config,
            provider_hint: Some("codex"),
            profile: None,
            additional_pre: Vec::new(),
            additional_post: Vec::new(),
            inline_pre: Vec::new(),
            wrap: None,
            provider_args: Vec::new(),
            capture_prompt: true,
            vars: HashMap::new(),
            session: SessionContext::default(),
            cwd: PathBuf::from("/tmp"),
        };

        let plan = build_pipeline(&request).expect("pipeline builds");
        assert!(
            plan.pipeline.contains("internal capture-arg"),
            "expected capture helper when capture enabled: {}",
            plan.pipeline
        );
        assert_eq!(
            plan.friendly_display, "codex --search",
            "expected friendly display to omit placeholder when source unknown"
        );
    }
}
