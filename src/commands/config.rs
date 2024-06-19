use crate::UserConfig;
use anyhow::bail;
use anyhow::ensure;
use anyhow::Context;
use std::path::Path;

#[derive(Debug, argh::FromArgs)]
#[argh(subcommand, name = "config", description = "interact with the config")]
pub struct Options {
    #[argh(switch, description = "whether to open the config file")]
    pub open: bool,

    #[argh(option, long = "key", short = 'k', description = "the key to update")]
    pub key: Option<String>,

    #[argh(
        option,
        long = "value",
        short = 'v',
        description = "the new value of the key"
    )]
    pub value: Option<String>,
}

pub async fn exec(
    options: Options,
    config_path: &Path,
    mut config: UserConfig,
) -> anyhow::Result<()> {
    if options.key.is_some() {
        ensure!(options.value.is_some(), "if a config key (--key, -k) is specified, a config value (--value, -v) must also be specified");
    }

    if options.value.is_some() {
        ensure!(options.key.is_some(), "if a config value (--value, -v) is specified, a config key (--key, -k) must also be specified");
    }

    if let (Some(key), Some(value)) = (options.key.as_deref(), options.value.as_deref()) {
        match key {
            "token" => {
                config.set_token(value);
            }
            _ => {
                bail!("key \"{key}\" is not recognized");
            }
        }

        crate::util::write_string_safe(&config_path, &config.to_string())
            .await
            .context("failed to write string")?;
    }

    if options.open {
        match tokio::fs::File::options()
            .create_new(true)
            .write(true)
            .open(&config_path)
            .await
        {
            Ok(_file) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).context("failed to create empty config file");
            }
        }

        opener::open(config_path)?;
    }

    Ok(())
}
