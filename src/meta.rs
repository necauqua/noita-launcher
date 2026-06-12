use chrono::TimeZone;
use color_eyre::Section;
use eyre::Context;
use eyre::Result;
use eyre::bail;
use eyre::eyre;
use serde::Deserialize;
use serde::Serialize;
use std::io::ErrorKind;
use std::path::Path;
use tokio::io::AsyncReadExt;

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

fn zero() -> u32 {
    0
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SaveMeta {
    pub instance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "zero", skip_serializing_if = "is_zero")]
    pub order: u32,
}

impl SaveMeta {
    pub fn new(instance: String) -> Self {
        Self {
            instance,
            description: None,
            order: 0,
        }
    }

    pub async fn read(save_dir: &Path) -> Result<Option<Self>> {
        let path = save_dir.join("meta.toml");
        match tokio::fs::read(&path).await {
            Ok(bytes) => toml::from_slice(&bytes).map(Some).map_err(|e| {
                eyre!(e).suggestion(format!("Delete or fix the file {}", path.display()))
            }),
            Err(e) if e.kind() == ErrorKind::NotFound => eyre::Ok(None),
            Err(e) => Err(e.into()),
        }
        .wrap_err_with(|| format!("Reading save metadata ({})", path.display()))
    }

    pub async fn write(&self, save_dir: &Path) -> Result<()> {
        let path = save_dir.join("meta.toml");
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
    #[serde(default = "zero", skip_serializing_if = "is_zero")]
    pub order: u32,

    #[serde(skip)]
    pub pe_timestamp: u32,
}

impl InstanceMeta {
    pub fn new(noita_args: Vec<String>, steam_manifest: u64) -> Self {
        Self {
            noita_args,
            steam_manifest,
            description: None,
            order: 0,
            pe_timestamp: 0,
        }
    }

    pub async fn read(instance_dir: &Path) -> Result<Self> {
        let path = instance_dir.join("meta.toml");
        let mut meta: Self = match tokio::fs::read(&path).await {
            Ok(bytes) => toml::from_slice(&bytes).map_err(|e| eyre!(e)),
            // Err(e) if e.kind() == ErrorKind::NotFound => {},
            Err(e) => Err(e.into()),
        }
        .wrap_err_with(|| format!("Reading instance metadata ({})", path.display()))?;

        let path = instance_dir.join("Noita").join("noita.exe");

        meta.pe_timestamp = async {
            let mut exe = tokio::fs::File::open(&path).await?;
            let mut header = vec![0u8; 4 * 1024];
            exe.read_exact(&mut header).await?;

            let pe_off = u32::from_le_bytes(header[0x3c..][..4].try_into()?) as usize;

            let Some(header) = header
                .get(pe_off..pe_off + 12)
                .filter(|h| &h[..4] == b"PE\0\0")
            else {
                bail!("Invalid PE header");
            };
            Ok(u32::from_le_bytes(header[8..12].try_into()?))
        }
        .await
        .wrap_err_with(|| format!("Reading PE timestamp from {}", path.display()))?;

        Ok(meta)
    }

    pub async fn write(&self, path: &Path) -> Result<()> {
        tokio::fs::write(&path, toml::to_string(self)?)
            .await
            .wrap_err_with(|| format!("Writing instance metadata ({})", path.display()))
    }

    pub fn human_timestamp(&self) -> String {
        chrono_tz::Europe::Helsinki
            .timestamp_opt(self.pe_timestamp as i64, 0)
            .unwrap()
            .format("%b %e %Y")
            .to_string()
    }
}
