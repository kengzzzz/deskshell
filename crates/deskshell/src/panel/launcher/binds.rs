//! Hyprland's binds as key chips, read each time the launcher opens.

use deskshell_core::binds;
use deskshell_hypr::{Bind, Client, Error};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindKeys {
    pub description: String,
    pub keys: Vec<String>,
}

pub async fn read(client: Client) -> Result<Vec<BindKeys>, Error> {
    let binds: Vec<Bind> = client.json("binds").await?;
    Ok(from_binds(binds))
}

pub fn from_binds(binds: Vec<Bind>) -> Vec<BindKeys> {
    binds
        .into_iter()
        .filter(|bind| !bind.description.is_empty())
        .map(|bind| BindKeys {
            keys: binds::keys(bind.modmask, &bind.key),
            description: bind.description,
        })
        .collect()
}

pub fn keys_for<'a>(binds: &'a [BindKeys], description: &str) -> &'a [String] {
    binds::keys_for(
        binds
            .iter()
            .map(|bind| (bind.description.as_str(), &bind.keys)),
        description,
    )
    .map_or(&[], Vec::as_slice)
}
