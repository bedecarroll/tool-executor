use clap::Args;
use color_eyre::eyre::WrapErr;

#[derive(Args, Debug)]
pub struct SelfUpdateCommand {
    /// Update to a specific release tag (defaults to the latest).
    #[arg(long, value_name = "TAG")]
    pub version: Option<String>,
}

pub fn run(cmd: SelfUpdateCommand) -> color_eyre::Result<()> {
    let mut builder = self_update::backends::github::Update::configure();
    builder
        .repo_owner("bedecarroll")
        .repo_name("llml")
        .bin_name("llml")
        .show_download_progress(true)
        .current_version(env!("CARGO_PKG_VERSION"));

    if let Some(tag) = cmd.version {
        builder.target_version_tag(&tag);
    }

    let status = builder
        .build()
        .wrap_err("failed to configure updater")?
        .update()
        .wrap_err("failed to apply update")?;
    tracing::info!(?status, "Completed self-update");

    Ok(())
}
