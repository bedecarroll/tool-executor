use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use directories::BaseDirs;
use indexmap::IndexMap;
use serde::Deserialize;
use shellexpand::full;
use std::env;
use std::path::PathBuf;
use toml::Value;

#[derive(Debug, Clone)]
pub struct Config {
    pub defaults: Defaults,
    pub providers: IndexMap<String, ProviderConfig>,
    pub snippets: SnippetConfig,
    pub wrappers: IndexMap<String, WrapperConfig>,
    pub profiles: IndexMap<String, ProfileConfig>,
    pub features: FeatureConfig,
}

#[derive(Debug, Clone)]
pub struct Defaults {
    pub provider: Option<String>,
    pub profile: Option<String>,
    pub search_mode: SearchMode,
    pub preview_filter: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    FirstPrompt,
    FullText,
}

impl SearchMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SearchMode::FirstPrompt => "first_prompt",
            SearchMode::FullText => "full_text",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub name: String,
    pub bin: String,
    pub flags: Vec<String>,
    pub env: Vec<EnvVar>,
    pub session_roots: Vec<PathBuf>,
    pub stdin: Option<StdinMapping>,
}

#[derive(Debug, Clone)]
pub struct EnvVar {
    pub key: String,
    pub value_template: String,
}

#[derive(Debug, Clone)]
pub struct StdinMapping {
    pub args: Vec<String>,
    pub mode: StdinMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdinMode {
    Pipe,
    CaptureArg,
}

#[derive(Debug, Clone)]
pub struct SnippetConfig {
    pub pre: IndexMap<String, Snippet>,
    pub post: IndexMap<String, Snippet>,
}

#[derive(Debug, Clone)]
pub struct Snippet {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone)]
pub struct WrapperConfig {
    pub name: String,
    pub mode: WrapperMode,
}

#[derive(Debug, Clone)]
pub enum WrapperMode {
    Shell { command: String },
    Exec { argv: Vec<String> },
}

#[derive(Debug, Clone)]
pub struct ProfileConfig {
    pub name: String,
    pub provider: String,
    pub description: Option<String>,
    pub pre: Vec<String>,
    pub post: Vec<String>,
    pub wrap: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FeatureConfig {
    pub prompt_assembler: Option<PromptAssemblerConfig>,
}

#[derive(Debug, Clone)]
pub struct PromptAssemblerConfig {
    pub namespace: String,
}

#[derive(Debug, Clone)]
pub struct ConfigDiagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Warning,
    Error,
}

impl Config {
    /// Parse a configuration [`Value`] into a [`Config`].
    ///
    /// # Errors
    ///
    /// Returns an error when the input TOML fails schema validation or cannot
    /// be decoded into the strongly typed configuration.
    pub fn from_value(value: &Value) -> Result<Self> {
        let raw: RawConfig = value
            .clone()
            .try_into()
            .map_err(|err: toml::de::Error| eyre!("failed to decode configuration: {err}"))?;
        raw.into_config()
    }

    #[must_use]
    pub fn lint(&self) -> Vec<ConfigDiagnostic> {
        let mut diags = Vec::new();
        if let Some(default_provider) = &self.defaults.provider
            && !self.providers.contains_key(default_provider)
        {
            diags.push(ConfigDiagnostic {
                level: DiagnosticLevel::Error,
                message: format!(
                    "default provider '{default_provider}' is not defined in [providers]"
                ),
            });
        }

        if let Some(default_profile) = &self.defaults.profile
            && !self.profiles.contains_key(default_profile)
        {
            diags.push(ConfigDiagnostic {
                level: DiagnosticLevel::Error,
                message: format!(
                    "default profile '{default_profile}' is not defined in [profiles]"
                ),
            });
        }

        for profile in self.profiles.values() {
            if !self.providers.contains_key(&profile.provider) {
                diags.push(ConfigDiagnostic {
                    level: DiagnosticLevel::Error,
                    message: format!(
                        "profile '{}' references unknown provider '{}'",
                        profile.name, profile.provider
                    ),
                });
            }

            for snippet in &profile.pre {
                if !self.snippets.pre.contains_key(snippet) {
                    diags.push(ConfigDiagnostic {
                        level: DiagnosticLevel::Warning,
                        message: format!(
                            "profile '{}' references unknown pre snippet '{}'",
                            profile.name, snippet
                        ),
                    });
                }
            }

            for snippet in &profile.post {
                if !self.snippets.post.contains_key(snippet) {
                    diags.push(ConfigDiagnostic {
                        level: DiagnosticLevel::Warning,
                        message: format!(
                            "profile '{}' references unknown post snippet '{}'",
                            profile.name, snippet
                        ),
                    });
                }
            }

            if let Some(wrapper) = &profile.wrap
                && !self.wrappers.contains_key(wrapper)
            {
                diags.push(ConfigDiagnostic {
                    level: DiagnosticLevel::Warning,
                    message: format!(
                        "profile '{}' references unknown wrapper '{}'",
                        profile.name, wrapper
                    ),
                });
            }
        }

        diags
    }
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default, flatten)]
    defaults: RawDefaults,
    #[serde(default)]
    providers: IndexMap<String, RawProvider>,
    #[serde(default)]
    snippets: RawSnippets,
    #[serde(default)]
    wrappers: IndexMap<String, RawWrapper>,
    #[serde(default)]
    profiles: IndexMap<String, RawProfile>,
    #[serde(default)]
    features: RawFeatures,
}

impl RawConfig {
    fn into_config(self) -> Result<Config> {
        let defaults = self.defaults.into_defaults()?;
        let mut providers = IndexMap::new();
        for (name, provider) in self.providers {
            providers.insert(name.clone(), provider.into_provider(name)?);
        }

        let snippets = self.snippets.into_snippets();

        let mut wrappers = IndexMap::new();
        for (name, wrapper) in self.wrappers {
            wrappers.insert(name.clone(), wrapper.into_wrapper(name)?);
        }

        let mut profiles = IndexMap::new();
        for (name, profile) in self.profiles {
            profiles.insert(name.clone(), profile.into_profile(name));
        }

        let features = self.features.into_features()?;

        Ok(Config {
            defaults,
            providers,
            snippets,
            wrappers,
            profiles,
            features,
        })
    }
}

#[derive(Debug, Deserialize, Default)]
struct RawDefaults {
    provider: Option<String>,
    profile: Option<String>,
    #[serde(default = "RawDefaults::default_search_mode")]
    search_mode: String,
    #[serde(default)]
    preview_filter: Option<CommandSpec>,
}

impl RawDefaults {
    fn default_search_mode() -> String {
        "first_prompt".to_string()
    }

    fn into_defaults(self) -> Result<Defaults> {
        let mode_key = self.search_mode.trim();
        let search_mode = match mode_key {
            "" | "first_prompt" => SearchMode::FirstPrompt,
            "full_text" => SearchMode::FullText,
            other => {
                return Err(eyre!("unknown search_mode '{other}'"));
            }
        };

        let preview_filter = match self.preview_filter {
            Some(spec) => spec
                .into_args()
                .map_err(|err| eyre!("invalid preview_filter command: {err}"))?,
            None => None,
        };

        Ok(Defaults {
            provider: self.provider,
            profile: self.profile,
            search_mode,
            preview_filter,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawProvider {
    bin: Option<String>,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    stdin_to: Option<String>,
    #[serde(default)]
    stdin_mode: Option<String>,
}

impl RawProvider {
    fn into_provider(self, name: String) -> Result<ProviderConfig> {
        let bin = self
            .bin
            .ok_or_else(|| eyre!("provider '{name}' is missing required field 'bin'"))?;
        let session_roots = infer_session_roots(&name);

        let stdin_mode = parse_stdin_mode(self.stdin_mode.as_deref())?;

        let stdin_args = match self.stdin_to {
            Some(ref raw) => parse_stdin(raw, &name)?,
            None => Vec::new(),
        };

        let stdin = if self.stdin_to.is_some() || !matches!(stdin_mode, StdinMode::Pipe) {
            Some(StdinMapping {
                args: stdin_args,
                mode: stdin_mode,
            })
        } else {
            None
        };

        Ok(ProviderConfig {
            name,
            bin,
            flags: self.flags,
            env: self
                .env
                .into_iter()
                .map(|entry| parse_env_var(&entry))
                .collect::<Result<Vec<_>>>()?,
            session_roots,
            stdin,
        })
    }
}

fn parse_env_var(raw: &str) -> Result<EnvVar> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| eyre!("environment entry '{raw}' must be in KEY=VALUE form"))?;
    if key.trim().is_empty() {
        return Err(eyre!("environment entry '{raw}' is missing a key"));
    }
    Ok(EnvVar {
        key: key.trim().to_string(),
        value_template: value.trim().to_string(),
    })
}

fn parse_command_args(raw: &str) -> Result<Vec<String>> {
    shlex::split(raw).ok_or_else(|| eyre!("failed to parse command line '{raw}'"))
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CommandSpec {
    String(String),
    List(Vec<String>),
}

impl CommandSpec {
    fn into_args(self) -> Result<Option<Vec<String>>> {
        match self {
            CommandSpec::String(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Ok(None);
                }
                let args = parse_command_args(trimmed)?;
                if args.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(args))
                }
            }
            CommandSpec::List(items) => {
                let args = items
                    .into_iter()
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>();
                if args.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(args))
                }
            }
        }
    }
}

fn parse_stdin(raw: &str, provider: &str) -> Result<Vec<String>> {
    if let Some((prefix, rest)) = raw.split_once(':') {
        if !prefix.trim().is_empty() && prefix.trim() != provider {
            return Err(eyre!(
                "stdin_to refers to provider '{prefix}' but is declared under provider '{provider}'"
            ));
        }
        parse_stdin(rest, provider)
    } else {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = parse_command_args(trimmed)?;
        if args.last().is_some_and(|last| last == "-") {
            args.pop();
        }
        Ok(args)
    }
}

fn parse_stdin_mode(raw: Option<&str>) -> Result<StdinMode> {
    let Some(value) = raw else {
        return Ok(StdinMode::Pipe);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(StdinMode::Pipe);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "pipe" => Ok(StdinMode::Pipe),
        "capture_arg" | "capture-arg" => Ok(StdinMode::CaptureArg),
        other => Err(eyre!("unknown stdin_mode '{other}'")),
    }
}

#[derive(Debug, Deserialize, Default)]
struct RawSnippets {
    #[serde(default)]
    pre: IndexMap<String, String>,
    #[serde(default)]
    post: IndexMap<String, String>,
}

impl RawSnippets {
    fn into_snippets(self) -> SnippetConfig {
        let pre = self
            .pre
            .into_iter()
            .map(|(name, command)| (name.clone(), Snippet { name, command }))
            .collect();

        let post = self
            .post
            .into_iter()
            .map(|(name, command)| (name.clone(), Snippet { name, command }))
            .collect();

        SnippetConfig { pre, post }
    }
}

#[derive(Debug, Deserialize)]
struct RawWrapper {
    #[serde(default)]
    shell: Option<bool>,
    cmd: Value,
}

impl RawWrapper {
    fn into_wrapper(self, name: String) -> Result<WrapperConfig> {
        let shell = self.shell.unwrap_or(false);
        let mode = if shell {
            let command = self
                .cmd
                .as_str()
                .ok_or_else(|| eyre!("wrapper '{name}' sets shell=true but cmd is not a string"))?
                .to_string();
            WrapperMode::Shell { command }
        } else {
            let array = self
                .cmd
                .as_array()
                .ok_or_else(|| {
                    eyre!("wrapper '{name}' expects cmd to be an array when shell=false")
                })?
                .iter()
                .map(|value| {
                    value.as_str().map(ToString::to_string).ok_or_else(|| {
                        eyre!("wrapper '{name}' cmd array must contain only strings")
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            WrapperMode::Exec { argv: array }
        };

        Ok(WrapperConfig { name, mode })
    }
}

#[derive(Debug, Deserialize)]
struct RawProfile {
    provider: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    pre: Vec<String>,
    #[serde(default)]
    post: Vec<String>,
    #[serde(default)]
    wrap: Option<String>,
}

impl RawProfile {
    fn into_profile(self, name: String) -> ProfileConfig {
        ProfileConfig {
            name,
            provider: self.provider,
            description: self
                .description
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            pre: self.pre,
            post: self.post,
            wrap: self.wrap,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct RawFeatures {
    #[serde(default)]
    pa: Option<RawPromptAssembler>,
}

impl RawFeatures {
    fn into_features(self) -> Result<FeatureConfig> {
        let prompt_assembler = match self.pa {
            Some(raw) if raw.enabled.unwrap_or(false) => Some(raw.into_config()?),
            _ => None,
        };

        Ok(FeatureConfig { prompt_assembler })
    }
}

#[derive(Debug, Deserialize, Default)]
struct RawPromptAssembler {
    enabled: Option<bool>,
    #[serde(default = "RawPromptAssembler::default_namespace")]
    namespace: String,
}

impl RawPromptAssembler {
    fn default_namespace() -> String {
        "pa".to_string()
    }

    #[allow(clippy::unnecessary_wraps)]
    fn into_config(self) -> Result<PromptAssemblerConfig> {
        Ok(PromptAssemblerConfig {
            namespace: self.namespace,
        })
    }
}

fn infer_session_roots(provider: &str) -> Vec<PathBuf> {
    match provider {
        "codex" => resolve_codex_session_roots(),
        _ => Vec::new(),
    }
}

fn resolve_codex_session_roots() -> Vec<PathBuf> {
    let mut homes = Vec::new();

    if let Ok(raw) = env::var("CODEX_HOME")
        && let Some(path) = expand_optional_path(&raw)
    {
        push_unique_path(&mut homes, path);
    }

    if let Some(base) = BaseDirs::new() {
        push_unique_path(&mut homes, base.home_dir().join(".codex"));
    }

    let mut roots = Vec::new();
    for home in homes {
        push_unique_path(&mut roots, home.join("session"));
        push_unique_path(&mut roots, home.join("sessions"));
    }

    roots
}

fn expand_optional_path(raw: &str) -> Option<PathBuf> {
    if raw.trim().is_empty() {
        return None;
    }
    expand_path(raw).ok()
}

fn push_unique_path(list: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !list.iter().any(|existing| existing == &candidate) {
        list.push(candidate);
    }
}

fn expand_path(raw: &str) -> Result<PathBuf> {
    let expanded = full(raw)
        .with_context(|| format!("failed to expand path '{raw}': environment variable missing"))?;
    Ok(PathBuf::from(expanded.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::Result;
    use indexmap::IndexMap;
    use toml::Value;

    #[test]
    fn config_from_value_requires_provider_bin() {
        let value: Value = toml::from_str(
            r#"
            [providers.codex]
            flags = ["--search"]
        "#,
        )
        .expect("parse toml");

        let error = Config::from_value(&value).expect_err("missing bin should error");
        let message = format!("{error:?}");
        assert!(message.contains("provider 'codex' is missing required field 'bin'"));
    }

    #[test]
    fn search_mode_as_str_reports_variants() {
        assert_eq!(SearchMode::FirstPrompt.as_str(), "first_prompt");
        assert_eq!(SearchMode::FullText.as_str(), "full_text");
    }

    #[test]
    fn raw_defaults_rejects_unknown_search_mode() {
        let defaults = RawDefaults {
            provider: None,
            profile: None,
            search_mode: "invalid".into(),
            preview_filter: None,
        };
        let err = defaults
            .into_defaults()
            .expect_err("unknown search mode should fail");
        assert!(
            err.to_string().contains("unknown search_mode 'invalid'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn config_from_value_parses_preview_filter_and_stdin_mode() {
        let value: Value = toml::from_str(
            r#"
            provider = "codex"
            search_mode = "full_text"
            preview_filter = "glow -s dark"

            [providers.codex]
            bin = "codex"
            stdin_mode = "capture-arg"
            stdin_to = "codex: jq -r .prompt -"

            [profiles.default]
            provider = "codex"
        "#,
        )
        .expect("parse toml");

        let config = Config::from_value(&value).expect("config parses");
        assert!(matches!(config.defaults.search_mode, SearchMode::FullText));
        let expected_filter = vec!["glow".to_string(), "-s".to_string(), "dark".to_string()];
        assert_eq!(
            config
                .defaults
                .preview_filter
                .as_ref()
                .expect("preview filter"),
            &expected_filter
        );

        let provider = config.providers.get("codex").expect("provider");
        let stdin = provider.stdin.as_ref().expect("stdin mapping");
        assert!(matches!(stdin.mode, StdinMode::CaptureArg));
        assert_eq!(stdin.args, vec!["jq", "-r", ".prompt"]);
    }

    #[test]
    fn config_from_value_rejects_malformed_env_entry() {
        let value: Value = toml::from_str(
            r#"
            [providers.codex]
            bin = "codex"
            env = ["=VALUE"]
        "#,
        )
        .expect("parse toml");

        let error = Config::from_value(&value).expect_err("env key required");
        let message = format!("{error:?}");
        assert!(message.contains("environment entry '=VALUE' is missing a key"));
    }

    #[test]
    fn parse_stdin_variants_cover_success_and_errors() {
        let args = parse_stdin("codex: jq .prompt -", "codex").expect("parse args");
        assert_eq!(args, vec!["jq", ".prompt"]);

        let error = parse_stdin("other: jq .prompt", "codex").expect_err("mismatch");
        let message = format!("{error:?}");
        assert!(message.contains("stdin_to refers to provider 'other'"));
    }

    #[test]
    fn parse_command_args_reports_invalid_syntax() {
        let error = parse_command_args("\"unterminated").expect_err("expected error");
        let message = format!("{error:?}");
        assert!(message.contains("failed to parse command line"));
    }

    #[test]
    fn config_lint_reports_unknown_entries() {
        let config = lint_fixture_config();
        let diagnostics = config.lint();
        let messages: Vec<_> = diagnostics
            .iter()
            .map(|diag| diag.message.as_str())
            .collect();

        assert!(
            messages
                .iter()
                .any(|msg| msg.contains("default provider 'missing'"))
        );
        assert!(
            messages
                .iter()
                .any(|msg| msg.contains("default profile 'absent-profile'"))
        );
        assert!(messages
            .iter()
            .any(|msg| msg.contains("profile 'default' references unknown pre snippet 'setup'")));
        assert!(messages.iter().any(|msg| {
            msg.contains("profile 'default' references unknown post snippet 'teardown'")
        }));
        assert!(messages.iter().any(|msg| {
            msg.contains("profile 'default' references unknown wrapper 'missing-wrap'")
        }));
    }

    #[test]
    fn config_lint_reports_unknown_provider_reference() {
        let mut config = lint_fixture_config();
        config.providers.clear();
        let messages: Vec<_> = config.lint().into_iter().map(|diag| diag.message).collect();
        assert!(
            messages
                .iter()
                .any(|msg| msg.contains("references unknown provider")),
            "missing provider diagnostic not found: {messages:?}"
        );
    }

    #[test]
    fn command_spec_string_splits_with_shlex() {
        let args = CommandSpec::String("glow -s dark".into())
            .into_args()
            .expect("parsing succeeds")
            .expect("non-empty command");
        assert_eq!(args, ["glow", "-s", "dark"]);
    }

    #[test]
    fn command_spec_list_trims_and_filters() {
        let args = CommandSpec::List(vec![
            " glow ".into(),
            " ".into(),
            "-s".into(),
            "dark".into(),
        ])
        .into_args()
        .expect("parsing succeeds")
        .expect("non-empty command");
        assert_eq!(args, ["glow", "-s", "dark"]);
    }

    #[test]
    fn command_spec_string_returns_none_for_blank() -> Result<()> {
        let result = CommandSpec::String("   ".into()).into_args()?;
        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn command_spec_list_returns_none_for_whitespace_only() -> Result<()> {
        let result = CommandSpec::List(vec![" ".into(), "\t".into()]).into_args()?;
        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn parse_stdin_returns_empty_for_blank_input() -> Result<()> {
        let args = parse_stdin("   ", "codex")?;
        assert!(args.is_empty());
        Ok(())
    }

    #[test]
    fn parse_stdin_mode_rejects_unknown_value() {
        let err = parse_stdin_mode(Some("weird")).unwrap_err();
        assert!(
            err.to_string().contains("unknown stdin_mode 'weird'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_env_var_parses_key_and_template() -> Result<()> {
        let var = parse_env_var("API_KEY=${env:API_KEY}")?;
        assert_eq!(var.key, "API_KEY");
        assert_eq!(var.value_template, "${env:API_KEY}");
        Ok(())
    }

    #[test]
    fn parse_env_var_rejects_missing_equals() {
        let err = parse_env_var("MISSING").unwrap_err();
        assert!(
            err.to_string()
                .contains("environment entry 'MISSING' must be in KEY=VALUE form")
        );
    }

    #[test]
    fn raw_wrapper_into_shell_mode() -> Result<()> {
        let wrapper = RawWrapper {
            shell: Some(true),
            cmd: Value::String("echo hello".into()),
        };
        let config = wrapper.into_wrapper("shellwrap".into())?;
        match config.mode {
            WrapperMode::Shell { command } => assert_eq!(command, "echo hello"),
            WrapperMode::Exec { .. } => panic!("expected shell mode"),
        }
        Ok(())
    }

    #[test]
    fn raw_wrapper_into_exec_mode() -> Result<()> {
        let wrapper = RawWrapper {
            shell: Some(false),
            cmd: Value::Array(vec![
                Value::String("ls".into()),
                Value::String("-la".into()),
            ]),
        };
        let config = wrapper.into_wrapper("execwrap".into())?;
        match config.mode {
            WrapperMode::Exec { argv } => assert_eq!(argv, vec!["ls", "-la"]),
            WrapperMode::Shell { .. } => panic!("expected exec mode"),
        }
        Ok(())
    }

    #[test]
    fn raw_wrapper_into_wrapper_reports_type_mismatch() {
        let wrapper = RawWrapper {
            shell: Some(true),
            cmd: Value::Array(vec![Value::String("ls".into())]),
        };
        let err = wrapper.into_wrapper("bad".into()).unwrap_err();
        assert!(
            err.to_string()
                .contains("wrapper 'bad' sets shell=true but cmd is not a string")
        );
    }

    fn lint_fixture_config() -> Config {
        let mut providers = IndexMap::new();
        providers.insert(
            "codex".into(),
            ProviderConfig {
                name: "codex".into(),
                bin: "codex".into(),
                flags: Vec::new(),
                env: Vec::new(),
                session_roots: Vec::new(),
                stdin: None,
            },
        );

        let mut profiles = IndexMap::new();
        profiles.insert(
            "default".into(),
            ProfileConfig {
                name: "default".into(),
                provider: "codex".into(),
                description: None,
                pre: vec!["setup".into()],
                post: vec!["teardown".into()],
                wrap: Some("missing-wrap".into()),
            },
        );

        Config {
            defaults: Defaults {
                provider: Some("missing".into()),
                profile: Some("absent-profile".into()),
                search_mode: SearchMode::FirstPrompt,
                preview_filter: None,
            },
            providers,
            snippets: SnippetConfig {
                pre: IndexMap::new(),
                post: IndexMap::new(),
            },
            wrappers: IndexMap::new(),
            profiles,
            features: FeatureConfig {
                prompt_assembler: None,
            },
        }
    }
}
