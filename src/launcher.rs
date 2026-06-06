use std::{io::ErrorKind, path::PathBuf};

use color_eyre::Section;
use eyre::{Context, OptionExt, bail, eyre};
use keyring_core::Entry;
use steam_vent::{
    Connection, ConnectionError, ServerList,
    auth::{
        AuthConfirmationHandler, ConfirmationAction, ConfirmationMethod, ConfirmationMethodClass,
        FileGuardDataStore, GuardTokenType, SteamGuardToken,
    },
};

use crate::{download::InstanceDownloader, printer::Printer, steam::Steam};

pub struct SteamTwoFactor {
    printer: Printer,
}

impl AuthConfirmationHandler for SteamTwoFactor {
    async fn handle_confirmation(
        self,
        allowed_confirmations: &[ConfirmationMethod],
    ) -> Option<ConfirmationAction> {
        if allowed_confirmations.iter().any(|m| !m.action_required()) {
            return Some(ConfirmationAction::None);
        }

        if let Some(method) = allowed_confirmations
            .iter()
            .find(|m| m.class() == ConfirmationMethodClass::Confirmation)
        {
            if method.confirmation_type() == "device confirmation" {
                self.printer.hint("[!!!] Open SteamGuard");
            } else {
                self.printer
                    .hint("[!!!] A confirmation link was sent to your email");
            }
            return Some(ConfirmationAction::None);
        }

        for method in allowed_confirmations {
            let Some(token_type) = method.token_type() else {
                continue;
            };
            match token_type {
                GuardTokenType::Email => self
                    .printer
                    .hint("A confirmation code was sent to your email"),
                GuardTokenType::Device => self
                    .printer
                    .hint("A confirmation code was sent to your device"),
            }

            let mut code = match inquire::Text::new("Code:").prompt() {
                Ok(c) => c,
                Err(_) => return Some(ConfirmationAction::Abort), // meh
            };

            code.truncate(code.trim().len());

            // ugly hack for now because there's no way to create a SteamGuardToken 🤦
            //  surely they'll fix this
            let token = unsafe { std::mem::transmute::<String, SteamGuardToken>(code) };

            return Some(ConfirmationAction::GuardToken(token, token_type));
        }
        None
    }
}

pub struct NoitaLauncher {
    app_dir: PathBuf,
    trampoline: PathBuf,
    hook_dll: PathBuf,
    cache_dir: PathBuf,
    printer: Printer,
    steam: Option<Steam>,
    http: Option<reqwest::Client>,
}

const APP_ID: u32 = 881100;
const DEPOT_ID: u32 = 881101;

impl NoitaLauncher {
    pub fn new(
        app_dir: PathBuf,
        trampoline: PathBuf,
        hook_dll: PathBuf,
        cache_dir: PathBuf,
        printer: Printer,
    ) -> Self {
        Self {
            app_dir,
            trampoline,
            hook_dll,
            cache_dir,
            printer,
            steam: None,
            http: None,
        }
    }

    fn keyring_entry() -> eyre::Result<Entry> {
        Entry::new("noita-launcher", "default").wrap_err("Failed to access the OS keyring")
    }

    async fn steam(&mut self) -> eyre::Result<&mut Steam> {
        // // Apparently Rust cant do this until polonius lmao
        // if let Some(steam) = &self.steam {
        //     return Ok(steam);
        // }

        if self.steam.is_some() {
            // icant
            return Ok(self.steam.as_mut().unwrap());
        }

        let entry = Self::keyring_entry()?;
        let connection = match entry.get_password() {
            Err(keyring_core::Error::NoEntry) => {
                return Err(eyre!("Not logged in")
                    .suggestion("Run `noita login` to login with your Steam account"));
            }
            data => async {
                let data = data?;
                let (account, token) = data
                    .split_once(':')
                    .ok_or_eyre("Invalid cretential stored in keyring")?;
                eyre::Ok(Connection::access(&ServerList::discover().await?, account, token).await?)
            }
            .await
            .suggestion("Run `noita logout && noita login` to fix this")?,
        };

        Ok(self
            .steam
            .insert(Steam::new(connection, self.cache_dir.join("manifests"))))
    }

    fn http(&mut self) -> reqwest::Client {
        match &self.http {
            Some(http) => http.clone(),
            _ => self.http.get_or_insert_default().clone(),
        }
    }

    pub async fn login(&mut self, username: &str, password: &str) -> eyre::Result<()> {
        let entry = Self::keyring_entry()?;

        let conn = match Connection::login(
            &ServerList::discover().await?,
            username,
            password,
            FileGuardDataStore::user_cache(),
            SteamTwoFactor {
                printer: self.printer.clone(),
            },
        )
        .await
        {
            Err(ConnectionError::Aborted) => return Ok(()),
            c => c?,
        };

        let token = conn
            .access_token()
            .ok_or_eyre("No access token from new connection")?;

        entry.set_password(&format!("{username}:{token}"))?;

        self.printer.hint("Successfully logged in");

        Ok(())
    }

    pub async fn logout(&mut self) -> eyre::Result<()> {
        match Self::keyring_entry()?.delete_credential() {
            Err(keyring_core::Error::NoEntry) => self.printer.hint("Was not logged in"),
            e => {
                e?;
                self.printer.hint("Logged out");
            }
        };
        self.steam = None;
        Ok(())
    }

    pub async fn run_instance(&mut self, instance: &str, save: &str) -> eyre::Result<()> {
        let instance_dir = self.app_dir.join("instances").join(instance);

        if !tokio::fs::try_exists(&instance_dir).await? {
            if instance != "main" {
                return Err(eyre!("instance '{}' does not exist", instance)
                    .suggestion("run `noita new` first"));
            }
            self.printer
                .hint("Default instance 'main' does not exist, setting it up..");
            self.new_instance(instance, None, None, false).await?;
        }
        let save_path = self.app_dir.join("saves").join(save);
        tokio::fs::create_dir_all(&save_path).await?;

        let noita_args = ["-no_logo_splashes"];

        #[cfg(windows)]
        {
            Command::new(&self.trampoline)
                .arg(instance_dir.join("noita.exe"))
                .arg(&self.hook_dll)
                .arg(save_path)
                .args(noita_args)
                .current_dir(instance_dir)
                .spawn()
                .note("Running on non-Windows requires umu-launcher to be installed (umu-run in PATH)")?
                .wait()
                .await?;
        }
        #[cfg(not(windows))]
        {
            use std::path::Path;

            use tokio::process::Command;

            fn to_wine(path: &Path) -> PathBuf {
                PathBuf::from("Z:\\").join(path.strip_prefix("/").unwrap_or(path))
            }

            Command::new("umu-run")
                .env("GAMEID", "umu-881100")
                .env("WINEPREFIX", self.app_dir.join("wineprefix"))
                .env("WINEDEBUG", "-all,+debugstr")
                .env("WINEDLLOVERRIDES", "winmm=n,b") // allow winmm.dll to be used for an asi loader
                .arg(&self.trampoline)
                .arg(&instance_dir)
                .arg("noita.exe")
                .arg(to_wine(&self.hook_dll))
                .arg(to_wine(&save_path))
                .args(noita_args)
                .current_dir(instance_dir)
                .spawn()
                .note("Running on non-Windows requires umu-launcher to be installed (umu-run in PATH)")?
                .wait()
                .await?;
        }
        Ok(())
    }

    pub async fn new_instance(
        &mut self,
        name: &str,
        manifest: Option<u64>,
        branch: Option<&str>,
        validate: bool,
    ) -> eyre::Result<()> {
        let instances_path = self.app_dir.join("instances");
        tokio::fs::create_dir_all(&instances_path).await?;

        let instance_path = instances_path.join(name);
        if tokio::fs::try_exists(&instance_path).await? {
            bail!("instance '{}' already exists", name);
        }

        let (size, downloader) = self.setup_downloader(manifest, branch).await?;

        let cache_bar = self.printer.bar(size).with_prefix("Checking cached chunks");
        let cached_size = downloader.compute_cached_size(validate, cache_bar).await?;

        let download_size = size - cached_size;

        if download_size == 0 {
            self.printer
                .hint("All chunks already cached, no download needed");
        }

        let temp_path = self.app_dir.join("temp").join(format!(
            "{name}.{}",
            std::iter::repeat_with(fastrand::alphanumeric)
                .take(8)
                .collect::<String>()
        ));

        let download_bar = self
            .printer
            .bar(download_size)
            .with_prefix("Downloading chunks");
        let write_bar = self.printer.bar(size).with_prefix("Writing instance files");

        downloader
            .fetch(&temp_path, download_bar, write_bar)
            .await?;

        if tokio::fs::try_exists(&instance_path).await? {
            tokio::fs::remove_dir_all(&temp_path).await?;
            bail!("instance '{name}' was created during the download, aborting",);
        }
        tokio::fs::rename(temp_path, instance_path).await?;

        Ok(())
    }

    pub async fn setup_downloader(
        &mut self,
        manifest: Option<u64>,
        branch: Option<&str>,
    ) -> eyre::Result<(u64, InstanceDownloader)> {
        let steam = self.steam().await?;

        let manifest = match manifest {
            Some(manifest) => manifest,
            None => {
                let current_release = steam.get_current_release(APP_ID, DEPOT_ID).await?;
                let branch = branch.unwrap_or("public");
                let branch = current_release
                    .branches
                    .iter()
                    .find(|b| b.name == branch)
                    .ok_or_eyre("Unknown Steam branch")?;
                branch.manifest
            }
        };

        let cdn_hosts = steam.get_cdn_hosts().await?;
        let depot = steam.get_depot(APP_ID, DEPOT_ID).await?;

        let (meta, payload) = steam
            .fetch_manifest(APP_ID, &depot, manifest, branch)
            .await?;

        let chunk_cache = self.cache_dir.join("chunks");
        tokio::fs::create_dir_all(&chunk_cache).await?;

        // eh we track uncompressed download size because cache's uncompressed
        let size = meta.cb_disk_original();

        Ok((
            size,
            InstanceDownloader::new(depot, payload, cdn_hosts, chunk_cache, self.http(), 32),
        ))
    }

    pub async fn prefetch(
        &mut self,
        manifest: Option<u64>,
        branch: Option<&str>,
        validate: bool,
    ) -> eyre::Result<()> {
        let (size, downloader) = self.setup_downloader(manifest, branch).await?;

        let bar = self.printer.bar(size).with_prefix("Checking cached chunks");
        let cached_size = downloader.compute_cached_size(validate, bar).await?;

        let download_size = size - cached_size;

        if download_size > 0 {
            let bar = self
                .printer
                .bar(download_size)
                .with_prefix("Downloading chunks");
            downloader.prefetch(bar).await?;
            self.printer.hint("Downloaded missing chunks");
        } else {
            self.printer
                .hint("All chunks were already cached, no download was needed");
        }

        Ok(())
    }

    pub async fn prefetch_all(&mut self, validate: bool) -> eyre::Result<()> {
        let versions = crate::meta::read().await?;

        let len = versions.len();
        for (i, version) in versions.into_iter().enumerate() {
            self.printer.hint(format!(
                "[{}/{len}] Prefetching version '{}'",
                i + 1,
                version.title,
            ));
            self.prefetch(Some(version.manifest), version.branch.as_deref(), validate)
                .await?;
        }

        Ok(())
    }

    pub async fn remove_instance(&mut self, name: &str) -> eyre::Result<()> {
        let instance_dir = self.app_dir.join("instances").join(name);
        if !tokio::fs::try_exists(&instance_dir).await? {
            bail!("instance '{name}' does not exist");
        }

        tokio::fs::remove_dir_all(instance_dir).await?;

        Ok(())
    }

    // todo: list some stats, like number of installed mods etc
    // ( + manifest id mb? get noita build string from noita.exe, or some _version_hash.txt matching bs)
    pub async fn list_instances(&self) -> eyre::Result<()> {
        let mut entries = match tokio::fs::read_dir(self.app_dir.join("instances")).await {
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            e => Some(e?),
        };

        let mut any = false;

        if let Some(entries) = &mut entries {
            while let Some(entry) = entries.next_entry().await? {
                any = true;
                println!("{}", entry.file_name().to_string_lossy());
            }
        }

        if !any {
            self.printer
                .hint("No instances found, run `noita new` to create one");
        }

        Ok(())
    }

    // todo list some save info, like
    //   global stats (d/w/cur/pb/playtime),
    //   local stats (current biome, session playtime, seed etc)
    pub async fn list_saves(&self) -> eyre::Result<()> {
        let mut entries = match tokio::fs::read_dir(self.app_dir.join("saves")).await {
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            e => Some(e?),
        };

        let mut any = false;

        if let Some(entries) = &mut entries {
            while let Some(entry) = entries.next_entry().await? {
                any = true;
                println!("{}", entry.file_name().to_string_lossy());
            }
        }

        if !any {
            self.printer
                .hint("No saves found, run an instance (with `noita run`)");
        }

        Ok(())
    }
}
