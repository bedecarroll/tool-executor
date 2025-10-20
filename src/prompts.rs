use std::process::Command;
use std::time::Instant;

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
}

#[derive(Debug)]
pub enum PromptStatus {
    Disabled,
    Ready {
        profiles: Vec<VirtualProfile>,
        cached: bool,
    },
    Unavailable {
        message: String,
    },
}

pub struct PromptAssembler {
    config: PromptAssemblerConfig,
    cache: Option<Cache>,
}

struct Cache {
    fetched_at: Instant,
    profiles: Vec<VirtualProfile>,
}

impl PromptAssembler {
    #[must_use]
    pub fn new(config: PromptAssemblerConfig) -> Self {
        Self {
            config,
            cache: None,
        }
    }

    pub fn refresh(&mut self, force: bool) -> PromptStatus {
        if force {
            self.cache = None;
        }

        if let Some(cache) = &self.cache
            && cache.fetched_at.elapsed() <= self.config.cache_ttl
        {
            return PromptStatus::Ready {
                profiles: cache.profiles.clone(),
                cached: true,
            };
        }

        match fetch_prompts(&self.config) {
            Ok(profiles) => {
                self.cache = Some(Cache {
                    fetched_at: Instant::now(),
                    profiles: profiles.clone(),
                });
                PromptStatus::Ready {
                    profiles,
                    cached: false,
                }
            }
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

    let entries = root
        .as_array()
        .ok_or_else(|| eyre!("unexpected JSON shape from pa; expected array"))?;

    let mut profiles = Vec::new();
    for entry in entries {
        if let Some(name) = entry.get("name").and_then(Value::as_str) {
            let description = entry
                .get("description")
                .or_else(|| entry.get("summary"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
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

            profiles.push(VirtualProfile {
                key: format!("{}/{}", config.namespace, name),
                name: name.to_string(),
                description,
                tags,
            });
        }
    }

    Ok(profiles)
}
