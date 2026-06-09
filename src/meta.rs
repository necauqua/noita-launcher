use std::io::ErrorKind;
use std::path::Path;

use color_eyre::Section;
use eyre::Context;
use eyre::Result;
use eyre::bail;
use eyre::eyre;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Deserialize)]
pub struct Version {
    pub title: String,
    pub manifest: u64,
    pub branch: Option<String>,
}

pub async fn read() -> Result<Vec<Version>> {
    #[derive(Debug, Deserialize)]
    struct Versions {
        pub version: Vec<Version>,
    }

    // this will eventually make a request to our server or something?.
    Ok(toml::from_str::<Versions>(include_str!("manifests.toml"))
        .wrap_err("bad embedded versions.toml")?
        .version)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SaveMeta {
    pub instance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl SaveMeta {
    pub async fn read(path: &Path) -> Result<Option<Self>> {
        match tokio::fs::read(&path).await {
            Ok(bytes) => toml::from_slice(&bytes).map(Some).map_err(|e| {
                eyre!(e).suggestion(format!("Delete or fix the file {}", path.display()))
            }),
            Err(e) if e.kind() == ErrorKind::NotFound => eyre::Ok(None),
            Err(e) => Err(e.into()),
        }
        .wrap_err_with(|| format!("Reading save metadata ({})", path.display()))
    }

    pub async fn write(&self, path: &Path) -> Result<()> {
        tokio::fs::write(&path, toml::to_string(self)?)
            .await
            .wrap_err_with(|| format!("Writing save metadata ({})", path.display()))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InstanceMeta {
    pub noita_args: Vec<String>,
    pub steam_manifest: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl InstanceMeta {
    pub fn get_pe_timestamp(data: &[u8]) -> Result<u32> {
        let pe_off = u32::from_le_bytes(data[0x3c..][..4].try_into()?) as usize;
        if &data[pe_off..][..4] != b"PE\0\0" {
            bail!("Invalid PE signature");
        }
        Ok(u32::from_le_bytes(data[pe_off + 8..][..4].try_into()?))
    }

    pub async fn read(path: &Path) -> Result<Self> {
        match tokio::fs::read(&path).await {
            Ok(bytes) => toml::from_slice(&bytes).map_err(|e| eyre!(e)),
            // Err(e) if e.kind() == ErrorKind::NotFound => {},
            Err(e) => Err(e.into()),
        }
        .wrap_err_with(|| format!("Reading instance metadata ({})", path.display()))
    }

    pub async fn write(&self, path: &Path) -> Result<()> {
        tokio::fs::write(&path, toml::to_string(self)?)
            .await
            .wrap_err_with(|| format!("Writing instance metadata ({})", path.display()))
    }
}
