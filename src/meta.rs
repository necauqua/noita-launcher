use eyre::Context;
use eyre::Result;
use serde::Deserialize;

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
