use crate::Config;

pub mod greet;
#[cfg(feature = "self-update")]
pub mod self_update;

#[derive(Debug, Clone)]
pub struct CommandContext<'a> {
    pub config: &'a Config,
}

impl<'a> CommandContext<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self { config }
    }
}
