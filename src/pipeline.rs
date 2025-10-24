use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use regex::Regex;
use shell_escape::unix::escape as shell_escape;
use std::sync::LazyLock;

use crate::config::model::{
    Config, ProviderConfig, Snippet, StdinMode, WrapperConfig, WrapperMode,
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

    let profile = match request.profile {
        Some(name) => Some(
            config
                .profiles
                .get(name)
                .ok_or_else(|| eyre!("profile '{name}' not found"))?,
        ),
        None => None,
    };

    let provider_name = profile
        .map(|p| p.provider.clone())
        .or_else(|| request.provider_hint.map(str::to_string))
        .or_else(|| config.defaults.provider.clone())
        .ok_or_else(|| eyre!("no provider selected; specify --profile or <provider>"))?;

    let provider = config
        .providers
        .get(&provider_name)
        .ok_or_else(|| eyre!("provider '{provider_name}' not defined"))?;

    let profile_wrap = profile.and_then(|p| p.wrap.clone());
    let wrap_name = request.wrap.map(str::to_string).or(profile_wrap);
    let wrapper = match wrap_name {
        Some(ref name) => Some(
            config
                .wrappers
                .get(name)
                .ok_or_else(|| eyre!("wrapper '{name}' not found"))?,
        ),
        None => None,
    };

    let mut pre_snippet_names = profile.map(|p| p.pre.clone()).unwrap_or_default();
    pre_snippet_names.extend(request.additional_pre.iter().cloned());

    let mut post_snippet_names = profile.map(|p| p.post.clone()).unwrap_or_default();
    post_snippet_names.extend(request.additional_post.iter().cloned());

    let pre_commands = resolve_snippets(&config.snippets.pre, &pre_snippet_names, "pre")?;
    let mut pre_commands = pre_commands;
    pre_commands.extend(request.inline_pre.iter().cloned());

    let post_commands = resolve_snippets(&config.snippets.post, &post_snippet_names, "post")?;

    let mut provider_args = provider.flags.clone();
    if let Some(stdin) = &provider.stdin {
        provider_args.extend(stdin.args.clone());
    }
    provider_args.extend(request.provider_args.iter().cloned());

    let mut stages = Vec::new();
    let capture_prompt = provider
        .stdin
        .as_ref()
        .is_some_and(|stdin| matches!(stdin.mode, StdinMode::CaptureArg));

    if capture_prompt {
        let internal_command = build_capture_command(provider, &pre_commands, &provider_args);
        stages.push(internal_command);
    } else {
        stages.extend(pre_commands.iter().cloned());
        let provider_stage = command_string(&provider.bin, &provider_args);
        stages.push(provider_stage);
    }
    stages.extend(post_commands.iter().cloned());

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

    Ok(PipelinePlan {
        pipeline,
        display,
        env,
        invocation,
        provider: provider.name.clone(),
        pre_snippets: pre_snippet_names,
        post_snippets: post_snippet_names,
        wrapper: wrap_name,
        cwd: request.cwd.clone(),
    })
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
    }
}
