use aes::Aes128;
use aes::cipher::KeyIvInit;
use aes::cipher::StreamCipher;
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

    #[serde(skip)]
    pub stats: Option<NoitaStats>,
}

impl SaveMeta {
    pub fn new(instance: String) -> Self {
        Self {
            instance,
            description: None,
            order: 0,
            stats: None,
        }
    }

    pub async fn read(save_dir: &Path) -> Result<Option<Self>> {
        let path = save_dir.join("meta.toml");
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let mut meta = toml::from_slice::<Self>(&bytes).map(Some).map_err(|e| {
                    eyre!(e).suggestion(format!("Delete or fix the file {}", path.display()))
                })?;

                if let Some(meta) = &mut meta {
                    meta.stats = NoitaStats::read(save_dir).await?;
                }

                Ok(meta)
            }
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

#[derive(Debug)]
pub struct NoitaStats {
    pub win_streak: u32,
    pub win_streak_pb: u32,
    pub wins: u32,
    pub deaths: u32,
}

impl NoitaStats {
    async fn read_salakieli(path: &Path) -> Result<Option<nxml_rs::Element>> {
        match tokio::fs::read(path).await {
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            e => async {
                let mut bytes = e?;

                // surely 64 bit counter is enough
                ctr::Ctr64BE::<Aes128>::new(b"SecretsOfTheAllS".into(), b"ThreeEyesAreWatc".into())
                    .apply_keystream(&mut bytes);

                let str = String::from_utf8(bytes.clone())?;
                let elem = nxml_rs::parse(&str)?.to_owned();

                eyre::Ok(Some(elem))
            }
            .await
            .wrap_err_with(|| format!("Reading Noita stats ({})", path.display())),
        }
    }

    pub async fn read(save_dir: &Path) -> Result<Option<Self>> {
        let stats_dir = save_dir
            .join("Nolla_Games_Noita")
            .join("save00")
            .join("stats");

        let stats = stats_dir.join("_stats.salakieli");
        let Some(stats) = Self::read_salakieli(&stats).await? else {
            return Ok(None);
        };

        let win_streak_pb = stats
            .as_ref()
            .child("highest")
            .and_then(|h| h.attr("streaks"))
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();

        let deaths = stats
            .as_ref()
            .child("global")
            .and_then(|h| h.attr("death_count"))
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();

        let wins = stats
            .as_ref()
            .child("KEY_VALUE_STATS")
            .map(|stats| {
                // let stats = stats
                //     .children("E")
                //     .filter_map(|e| Some((e.attr("key")?, e.attr("value")?)))
                //     .collect::<HashMap<_, _>>();

                // let endroom_wins = stats
                //     .get("progress_ending0")
                //     .and_then(|s| s.parse::<u32>().ok())
                //     .unwrap_or_default();

                // let altar_wins = stats
                //     .get("progress_ending1")
                //     .and_then(|s| s.parse::<u32>().ok())
                //     .unwrap_or_default();

                // endroom_wins + altar_wins

                let mut wins = 0;

                for e in stats.children("E") {
                    if let Some("progress_ending0") | Some("progress_ending1") = e.attr("key") {
                        wins += e
                            .attr("value")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or_default();
                    }
                }

                wins
            })
            .unwrap_or_default();

        let win_streak = Self::read_salakieli(&stats_dir.join("_streaks.salakieli"))
            .await?
            .as_ref()
            .and_then(|gs| gs.attr("current_streak_count"))
            .and_then(|c| c.parse::<u32>().ok())
            .unwrap_or_default();

        if win_streak + win_streak_pb + wins + deaths == 0 {
            return Ok(None);
        }

        Ok(Some(NoitaStats {
            win_streak,
            win_streak_pb,
            wins,
            deaths,
        }))
    }
}
