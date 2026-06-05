use std::sync::Arc;

use eyre::OptionExt;
use eyre::Result;
use eyre::bail;
use tokio::sync::Semaphore;

pub struct CdnDownloaderState {
    hosts: Arc<[String]>,
    http: reqwest::Client,
    limiter: Semaphore,
}

pub struct CdnDownloader(Arc<CdnDownloaderState>);

impl CdnDownloader {
    pub fn new(hosts: Arc<[String]>, http: reqwest::Client, parallelism: usize) -> Self {
        Self(Arc::new(CdnDownloaderState {
            hosts,
            http,
            limiter: Semaphore::new(parallelism),
        }))
    }

    async fn try_download(&self, url: &str, size_hint: Option<usize>) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(size_hint.unwrap_or(128 * 1024));

        let permit = self.0.limiter.acquire().await?;

        let mut resp = self.0.http.get(url).send().await?;
        if !resp.status().is_success() {
            bail!("HTTP error: {}", resp.status());
        }

        while let Some(chunk) = resp.chunk().await? {
            bytes.extend_from_slice(&chunk);
        }

        drop(permit);

        Ok(bytes)
    }

    pub async fn download<F, Fut, R>(
        &self,
        url_path: &str,
        size_hint: Option<usize>,
        mut postprocess: F,
    ) -> Result<R>
    where
        F: FnMut(Vec<u8>) -> Fut,
        Fut: Future<Output = Result<R>> + Send,
    {
        const ATTEMPTS_PER_HOST: u32 = 2;

        let mut hosts = &self.0.hosts[..];
        let mut attempts = ATTEMPTS_PER_HOST;

        loop {
            let (host, rest) = hosts
                .split_first()
                .ok_or_eyre("No CDN could fulfill the download")?;

            let url = format!("https://{host}/{url_path}");

            let e = match self.try_download(&url, size_hint).await {
                Ok(bytes) => match postprocess(bytes).await {
                    Ok(bytes) => return Ok(bytes),
                    Err(e) => e,
                },
                Err(e) => e,
            };

            attempts -= 1;
            if attempts != 0 {
                tracing::warn!(error = ?e, "failed to download from {url}, retrying");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            attempts = ATTEMPTS_PER_HOST;
            hosts = rest;
            tracing::warn!(error = ?e, "failed to download from {url}, trying next host");
        }
    }
}
