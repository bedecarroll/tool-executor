use std::borrow::Cow;
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};
use serde_json::Value;

use crate::config::model::PromptAssemblerConfig;

#[derive(Debug, Clone)]
pub struct VirtualProfile {
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub stdin_supported: bool,
}

#[derive(Debug)]
pub enum PromptStatus {
    Disabled,
    Ready { profiles: Vec<VirtualProfile> },
    Unavailable { message: String },
}

pub struct PromptAssembler {
    config: PromptAssemblerConfig,
}

impl PromptAssembler {
    #[must_use]
    pub fn new(config: PromptAssemblerConfig) -> Self {
        Self { config }
    }

    pub fn refresh(&mut self, _force: bool) -> PromptStatus {
        match fetch_prompts(&self.config) {
            Ok(profiles) => PromptStatus::Ready { profiles },
            Err(err) => PromptStatus::Unavailable {
                message: format!("prompt assembler unavailable: {err:#}")
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            },
        }
    }
}

fn fetch_prompts(config: &PromptAssemblerConfig) -> Result<Vec<VirtualProfile>> {
    let output = Command::new("pa")
        .args(["list", "--json"])
        .output()
        .context("failed to execute 'pa list --json'")?;

    if !output.status.success() {
        return Err(eyre!("pa exited with status {}", output.status));
    }

    let root: Value =
        serde_json::from_slice(&output.stdout).context("failed to parse JSON output from pa")?;

    let entries = if let Some(array) = root.as_array() {
        Cow::Borrowed(array)
    } else if let Some(array) = root.get("prompts").and_then(Value::as_array) {
        Cow::Borrowed(array)
    } else {
        return Err(eyre!(
            "unexpected JSON shape from pa; expected an array or object with 'prompts'"
        ));
    };

    let mut profiles = Vec::new();
    for entry in entries.iter() {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        let description = entry
            .get("description")
            .or_else(|| entry.get("summary"))
            .and_then(Value::as_str)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let tags = entry
            .get("tags")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let stdin_supported = entry
            .get("stdin_supported")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        profiles.push(VirtualProfile {
            key: format!("{}/{}", config.namespace, name),
            name: name.to_string(),
            description,
            tags,
            stdin_supported,
        });
    }

    Ok(profiles)
}
