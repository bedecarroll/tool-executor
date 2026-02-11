use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use directories::BaseDirs;
use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, generate::SchemaGenerator, json_schema};
use serde::Deserialize;
use serde::de::{self, Deserializer};
use shellexpand::full;
use std::borrow::Cow;
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
    pub terminal_title: Option<String>,
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
    pub prompt_assembler: Option<String>,
    pub prompt_assembler_args: Vec<String>,
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

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct RawConfig {
    #[serde(default, flatten)]
    defaults: RawDefaults,
    #[serde(default)]
    #[schemars(with = "std::collections::BTreeMap<String, RawProvider>")]
    providers: IndexMap<String, RawProvider>,
    #[serde(default)]
    snippets: RawSnippets,
    #[serde(default)]
    #[schemars(with = "std::collections::BTreeMap<String, RawWrapper>")]
    wrappers: IndexMap<String, RawWrapper>,
    #[serde(default)]
    #[schemars(with = "std::collections::BTreeMap<String, RawProfile>")]
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

#[derive(Debug, Deserialize, Default, JsonSchema)]
pub(crate) struct RawDefaults {
    provider: Option<String>,
    profile: Option<String>,
    #[serde(default = "RawDefaults::default_search_mode")]
    search_mode: String,
    terminal_title: Option<String>,
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

        Ok(Defaults {
            provider: self.provider,
            profile: self.profile,
            search_mode,
            terminal_title: self.terminal_title,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawStdinMode {
    Pipe,
    CaptureArg,
}

impl Default for RawStdinMode {
    fn default() -> Self {
        Self::Pipe
    }
}

impl RawStdinMode {
    fn into_mode(self) -> StdinMode {
        match self {
            RawStdinMode::Pipe => StdinMode::Pipe,
            RawStdinMode::CaptureArg => StdinMode::CaptureArg,
        }
    }

    fn parse_token(raw: &str) -> Option<Self> {
        if raw.is_empty() {
            return Some(Self::Pipe);
        }
        let normalized = raw.to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "pipe" => Some(Self::Pipe),
            "capture_arg" => Some(Self::CaptureArg),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for RawStdinMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let trimmed = raw.trim();
        RawStdinMode::parse_token(trimmed)
            .ok_or_else(|| de::Error::unknown_variant(trimmed, &["pipe", "capture_arg"]))
    }
}

impl JsonSchema for RawStdinMode {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("RawStdinMode")
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "enum": ["pipe", "capture_arg"],
            "default": "pipe",
            "description": "Controls how tx streams prompts to provider executables. Use 'capture_arg' to pass the prompt as an argument."
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct RawProvider {
    bin: Option<String>,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    stdin_to: Option<String>,
    #[serde(default)]
    stdin_mode: RawStdinMode,
}

impl RawProvider {
    fn into_provider(self, name: String) -> Result<ProviderConfig> {
        let bin = self
            .bin
            .ok_or_else(|| eyre!("provider '{name}' is missing required field 'bin'"))?;
        let session_roots = infer_session_roots(&name);

        let stdin_mode = self.stdin_mode.into_mode();

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

#[derive(Debug, Deserialize, Default, JsonSchema)]
pub(crate) struct RawSnippets {
    #[serde(default)]
    #[schemars(with = "std::collections::BTreeMap<String, String>")]
    pre: IndexMap<String, String>,
    #[serde(default)]
    #[schemars(with = "std::collections::BTreeMap<String, String>")]
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum WrapperCommandSpec {
    String(String),
    List(Vec<String>),
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct RawWrapper {
    #[serde(default)]
    shell: Option<bool>,
    cmd: WrapperCommandSpec,
}

impl RawWrapper {
    fn into_wrapper(self, name: String) -> Result<WrapperConfig> {
        let shell = self.shell.unwrap_or(false);
        let mode = if shell {
            let command = match self.cmd {
                WrapperCommandSpec::String(value) => value,
                WrapperCommandSpec::List(_) => {
                    return Err(eyre!(
                        "wrapper '{name}' sets shell=true but cmd is not a string"
                    ));
                }
            };
            WrapperMode::Shell { command }
        } else {
            let argv = match self.cmd {
                WrapperCommandSpec::List(values) => values,
                WrapperCommandSpec::String(_) => {
                    return Err(eyre!(
                        "wrapper '{name}' expects cmd to be an array when shell=false"
                    ));
                }
            };
            WrapperMode::Exec { argv }
        };

        Ok(WrapperConfig { name, mode })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct RawProfile {
    provider: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    pre: Vec<String>,
    #[serde(default)]
    post: Vec<String>,
    #[serde(default)]
    wrap: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Prompt name to render via the prompt-assembler helper before launching the provider."
    )]
    prompt_assembler: Option<String>,
    #[serde(default)]
    prompt_assembler_args: Vec<String>,
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
            prompt_assembler: self
                .prompt_assembler
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            prompt_assembler_args: self.prompt_assembler_args,
        }
    }
}

#[derive(Debug, Deserialize, Default, JsonSchema)]
pub(crate) struct RawFeatures {
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

#[derive(Debug, Deserialize, Default, JsonSchema)]
pub(crate) struct RawPromptAssembler {
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
    fn config_lint_reports_missing_defaults_and_profile_references() {
        let defaults = Defaults {
            provider: Some("codex".into()),
            profile: Some("demo".into()),
            search_mode: SearchMode::FirstPrompt,
            terminal_title: None,
        };

        let snippets = SnippetConfig {
            pre: IndexMap::new(),
            post: IndexMap::new(),
        };

        let wrappers = IndexMap::new();

        let mut profiles = IndexMap::new();
        profiles.insert(
            "demo".into(),
            ProfileConfig {
                name: "demo".into(),
                provider: "missing-provider".into(),
                description: None,
                pre: vec!["prep".into()],
                post: vec!["cleanup".into()],
                wrap: Some("shellwrap".into()),
                prompt_assembler: None,
                prompt_assembler_args: Vec::new(),
            },
        );

        let config = Config {
            defaults,
            providers: IndexMap::new(),
            snippets,
            wrappers,
            profiles,
            features: FeatureConfig {
                prompt_assembler: None,
            },
        };

        let diagnostics = config.lint();
        assert!(diagnostics.iter().any(|diag| {
            diag.message
                .contains("default provider 'codex' is not defined in [providers]")
        }));
        assert!(diagnostics.iter().any(|diag| {
            diag.message
                .contains("profile 'demo' references unknown provider 'missing-provider'")
        }));
        assert!(diagnostics.iter().any(|diag| {
            diag.message
                .contains("profile 'demo' references unknown pre snippet 'prep'")
        }));
        assert!(diagnostics.iter().any(|diag| {
            diag.message
                .contains("profile 'demo' references unknown post snippet 'cleanup'")
        }));
        assert!(diagnostics.iter().any(|diag| {
            diag.message
                .contains("profile 'demo' references unknown wrapper 'shellwrap'")
        }));
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
            terminal_title: None,
        };
        let err = defaults
            .into_defaults()
            .expect_err("unknown search mode should fail");
        assert!(err.to_string().contains("unknown search_mode 'invalid'"));
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
                .any(|msg| msg.contains("references unknown provider"))
        );
    }

    #[test]
    fn expand_optional_path_returns_none_for_blank_input() {
        assert_eq!(expand_optional_path("   "), None);
    }

    #[test]
    fn parse_stdin_returns_empty_for_blank_input() -> Result<()> {
        let args = parse_stdin("   ", "codex")?;
        assert!(args.is_empty());
        Ok(())
    }

    #[test]
    fn raw_stdin_mode_resolves_aliases() {
        let capture: RawStdinMode = serde_json::from_str("\"capture-arg\"").expect("deserialize");
        assert!(matches!(capture.into_mode(), StdinMode::CaptureArg));

        let pipe: RawStdinMode = serde_json::from_str("\"PIPE\"").expect("deserialize");
        assert!(matches!(pipe.into_mode(), StdinMode::Pipe));
    }

    #[test]
    fn raw_stdin_mode_rejects_unknown_value() {
        let err = serde_json::from_str::<RawStdinMode>("\"weird\"").unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn raw_stdin_mode_empty_token_defaults_to_pipe() {
        let mode = RawStdinMode::parse_token("").expect("empty token should parse");
        assert!(matches!(mode.into_mode(), StdinMode::Pipe));
    }

    #[test]
    fn raw_snippets_into_snippets_maps_pre_and_post_entries() {
        let mut pre = IndexMap::new();
        pre.insert("setup".to_string(), "echo pre".to_string());
        let mut post = IndexMap::new();
        post.insert("teardown".to_string(), "echo post".to_string());
        let snippets = RawSnippets { pre, post }.into_snippets();
        assert_eq!(
            snippets
                .pre
                .get("setup")
                .map(|snippet| snippet.command.as_str()),
            Some("echo pre")
        );
        assert_eq!(
            snippets
                .post
                .get("teardown")
                .map(|snippet| snippet.command.as_str()),
            Some("echo post")
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
            cmd: WrapperCommandSpec::String("echo hello".into()),
        };
        let config = wrapper.into_wrapper("shellwrap".into())?;
        assert!(matches!(
            config.mode,
            WrapperMode::Shell { ref command } if command == "echo hello"
        ));
        Ok(())
    }

    #[test]
    fn raw_wrapper_into_exec_mode() -> Result<()> {
        let wrapper = RawWrapper {
            shell: Some(false),
            cmd: WrapperCommandSpec::List(vec!["ls".into(), "-la".into()]),
        };
        let config = wrapper.into_wrapper("execwrap".into())?;
        assert!(matches!(
            config.mode,
            WrapperMode::Exec { ref argv }
                if argv.len() == 2 && argv[0] == "ls" && argv[1] == "-la"
        ));
        Ok(())
    }

    #[test]
    fn raw_wrapper_into_wrapper_reports_type_mismatch() {
        let wrapper = RawWrapper {
            shell: Some(true),
            cmd: WrapperCommandSpec::List(vec!["ls".into()]),
        };
        let err = wrapper.into_wrapper("bad".into()).unwrap_err();
        assert!(
            err.to_string()
                .contains("wrapper 'bad' sets shell=true but cmd is not a string")
        );
    }

    #[test]
    fn raw_wrapper_into_wrapper_reports_exec_type_mismatch() {
        let wrapper = RawWrapper {
            shell: Some(false),
            cmd: WrapperCommandSpec::String("echo hi".into()),
        };
        let err = wrapper.into_wrapper("bad-exec".into()).unwrap_err();
        assert!(
            err.to_string()
                .contains("wrapper 'bad-exec' expects cmd to be an array when shell=false")
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
                prompt_assembler: None,
                prompt_assembler_args: Vec::new(),
            },
        );

        Config {
            defaults: Defaults {
                provider: Some("missing".into()),
                profile: Some("absent-profile".into()),
                search_mode: SearchMode::FirstPrompt,
                terminal_title: None,
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
