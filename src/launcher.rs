use std::io::ErrorKind;
use std::path::PathBuf;

use color_eyre::Section;
use eyre::Context;
use eyre::OptionExt;
use keyring_core::Entry;
use steam_vent::Connection;
use steam_vent::ConnectionError;
use steam_vent::ServerList;
use steam_vent::auth::AuthConfirmationHandler;
use steam_vent::auth::ConfirmationAction;
use steam_vent::auth::ConfirmationMethod;
use steam_vent::auth::ConfirmationMethodClass;
use steam_vent::auth::FileGuardDataStore;
use steam_vent::auth::GuardTokenType;
use steam_vent::auth::SteamGuardToken;
use steam_vent_proto::content_manifest::content_manifest_payload::FileMapping;

use crate::download::InstanceDownloader;
use crate::meta::InstanceMeta;
use crate::meta::SaveMeta;
use crate::printer::Printer;
use crate::steam::Steam;
use crate::user_bail;

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

// The folder setup is the following as of now:
//   cache_dir/
//     chunks/xy/z... - chunk cache, steam depot chunks organized by first 2 chars of sha1
//     manifests/<manifest_id> - steam manifest cache
//
//   app_dir/
//     noita-path-hook.dll
//     noita-trampoline.exe
//     wineprefix/ - WINEPREFIX on linux
//     temp/<name>.<random>/ - temporary instance dirs used during download
//     instances/<name>/ - instance dir, cwd for a particular Noita version
//       meta.toml - our per-instance settings and metadata
//       Noita/.. - game cwd
//     saves/<name>/ - a single save dir
//       meta.toml - our per-save settings
//       Nolla_Games_Noita/.. - Noita seed the save folder as the appdata, so it uses this folder

pub struct NoitaLauncher {
    app_dir: PathBuf,
    trampoline: PathBuf,
    hook_dll: PathBuf,
    cache_dir: PathBuf,
    printer: Printer,
    steam: Option<Steam>,
    http: Option<reqwest::Client>,
    downloader: Option<InstanceDownloader>,
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
            downloader: None,
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
                user_bail!(
                    "Not logged in",
                    hint = "Run `noita login` to login with your Steam account",
                );
            }
            data => {
                async {
                    let data = data?;
                    let (account, token) = data
                        .split_once(':')
                        .ok_or_eyre("Invalid cretential stored in keyring")
                        .suggestion("Run `noita logout && noita login` to fix this")?;
                    eyre::Ok(
                        Connection::access(&ServerList::discover().await?, account, token).await?,
                    )
                }
                .await?
            }
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

    async fn downloader(&mut self) -> eyre::Result<InstanceDownloader> {
        // and again
        if self.downloader.is_some() {
            return Ok(self.downloader.as_mut().unwrap().clone());
        }

        let steam = self.steam().await?;
        let depot = steam.get_depot(APP_ID, DEPOT_ID).await?;
        let cdn_hosts = steam.get_cdn_hosts().await?;
        let http = self.http();

        let chunk_cache = self.cache_dir.join("chunks");
        tokio::fs::create_dir_all(&chunk_cache).await?;

        Ok(self
            .downloader
            .insert(InstanceDownloader::new(
                depot,
                cdn_hosts,
                chunk_cache,
                http,
                32,
            ))
            .clone())
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

    pub async fn run_instance(
        &mut self,
        instance: &str,
        save: &str,
        force: bool,
    ) -> eyre::Result<()> {
        let instance_dir = self.app_dir.join("instances").join(instance);

        let mut fresh = false;

        let meta = if !tokio::fs::try_exists(&instance_dir).await? {
            if instance != "main" {
                user_bail!(
                    "Instance '{instance}' does not exist",
                    hint = "Run `noita new` first",
                );
            }
            self.printer
                .hint("Default instance 'main' does not exist, setting it up..");
            fresh = true;
            self.new_instance(instance, None, None, false).await?
        } else {
            InstanceMeta::read(&instance_dir).await?
        };

        let save_path = self.app_dir.join("saves").join(save);
        tokio::fs::create_dir_all(&save_path)
            .await
            .wrap_err_with(|| format!("Creating dir ({})", save_path.display()))?;

        if let Some(mut meta) = SaveMeta::read(&save_path).await? {
            if meta.instance != instance {
                if force {
                    meta.instance = instance.into();
                    meta.write(&save_path).await?;
                    self.printer.warn(format!(
                        "Overriding save '{save}' to be associated with instance '{instance}' (was associated with '{}')", meta.instance
                    ));
                } else {
                    user_bail!(
                        "Save '{save}' is associated with a different instance",
                        hint = "Use `noita run -f/--force` to ignore this check (this will override the associated instance)",
                    );
                }
            }
        } else {
            if !fresh {
                self.printer.warn(format!(
                    "Save {save} did not have meta.toml, creating one associated with '{instance}'"
                ));
            }
            SaveMeta::new(instance.into()).write(&save_path).await?
        }

        #[cfg(windows)]
        {
            Command::new(&self.trampoline)
                .arg(&self.hook_dll)
                .arg(save_path)
                .arg("noita.exe")
                .args(meta.noita_args)
                .current_dir(instance_dir.join("Noita"))
                .spawn()?
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
                .env("UMU_RUNTIME_UPDATE", "0")
                .env("GAMEID", "umu-881100")
                .env("WINEPREFIX", self.app_dir.join("wineprefix"))
                .env("WINEDEBUG", "-all,+debugstr")
                .env("WINEDLLOVERRIDES", "winmm=n,b") // allow winmm.dll to be used for an asi loader
                .arg(&self.trampoline)
                .arg(to_wine(&self.hook_dll))
                .arg(to_wine(&save_path))
                .arg("noita.exe")
                .args(meta.noita_args)
                .current_dir(instance_dir.join("Noita"))
                .spawn()
                .wrap_err("Running the game with umu-run")
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
    ) -> eyre::Result<InstanceMeta> {
        let instances_path = self.app_dir.join("instances");
        tokio::fs::create_dir_all(&instances_path).await?;

        let instance_path = instances_path.join(name);
        if tokio::fs::try_exists(&instance_path).await? {
            user_bail!("Instance '{name}' already exists");
        }

        let (size, manifest, mappings) = self.prepare_download(manifest, branch).await?;
        let downloader = self.downloader().await?;

        let cache_bar = self.printer.bar(size).with_prefix("Checking cached chunks");
        let cached_size = downloader
            .compute_cached_size(&mappings, validate, cache_bar)
            .await?;

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
            .fetch(mappings, &temp_path, download_bar, write_bar)
            .await?;

        let instance_cwd = instance_path.join("Noita");

        if tokio::fs::try_exists(&instance_cwd).await? {
            tokio::fs::remove_dir_all(&temp_path).await?;
            user_bail!("Instance '{name}' was created during the download, aborting",);
        }
        tokio::fs::create_dir_all(&instance_path).await?;
        tokio::fs::rename(temp_path, instance_cwd).await?;

        let meta = InstanceMeta::new(vec!["-no_logo_splashes".into()], manifest);
        meta.write(&instance_path.join("meta.toml")).await?;
        Ok(meta)
    }

    pub async fn prepare_download(
        &mut self,
        manifest: Option<u64>,
        branch: Option<&str>,
    ) -> eyre::Result<(u64, u64, Vec<FileMapping>)> {
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

        let depot = steam.get_depot(APP_ID, DEPOT_ID).await?;

        let (meta, payload) = steam
            .fetch_manifest(APP_ID, &depot, manifest, branch)
            .await?;

        // eh we track uncompressed download size because cache's uncompressed
        let size = meta.cb_disk_original();

        Ok((size, manifest, payload.mappings))
    }

    pub async fn prefetch(
        &mut self,
        manifest: Option<u64>,
        branch: Option<&str>,
        validate: bool,
    ) -> eyre::Result<()> {
        let (size, _, mappings) = self.prepare_download(manifest, branch).await?;
        let downloader = self.downloader().await?;

        let bar = self.printer.bar(size).with_prefix("Checking cached chunks");
        let cached_size = downloader
            .compute_cached_size(&mappings, validate, bar)
            .await?;

        let download_size = size - cached_size;

        if download_size > 0 {
            let bar = self
                .printer
                .bar(download_size)
                .with_prefix("Downloading chunks");
            downloader.prefetch(&mappings, bar).await?;
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

    pub async fn remove_instances(&mut self, names: &[String]) -> eyre::Result<()> {
        for name in names {
            let instance_dir = self.app_dir.join("instances").join(name);
            if tokio::fs::try_exists(&instance_dir).await? {
                tokio::fs::remove_dir_all(instance_dir).await?;
            } else {
                self.printer
                    .warn(format!("Instance '{name}' does not exist"));
            }
        }

        Ok(())
    }

    // todo: list some stats, like number of installed mods etc
    // ( + manifest id mb? get noita build string from noita.exe, or some _version_hash.txt matching bs)
    pub async fn list_instances(&self) -> eyre::Result<Vec<(String, InstanceMeta)>> {
        let mut result = vec![];

        let mut entries = match tokio::fs::read_dir(self.app_dir.join("instances")).await {
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            e => Some(e?),
        };
        if let Some(entries) = &mut entries {
            while let Some(entry) = entries.next_entry().await? {
                result.push((
                    entry.file_name().to_string_lossy().into_owned(),
                    InstanceMeta::read(&entry.path()).await?,
                ));
            }
        }

        Ok(result)
    }

    // todo list some save info, like
    //   global stats (d/w/cur/pb/playtime),
    //   local stats (current biome, session playtime, seed etc)
    pub async fn list_saves(&self) -> eyre::Result<Vec<(String, SaveMeta)>> {
        let mut entries = match tokio::fs::read_dir(self.app_dir.join("saves")).await {
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            e => Some(e?),
        };

        let mut result = vec![];

        if let Some(entries) = &mut entries {
            while let Some(entry) = entries.next_entry().await? {
                result.push((
                    entry.file_name().to_string_lossy().into_owned(),
                    match SaveMeta::read(&entry.path()).await? {
                        Some(meta) => meta,
                        None => continue,
                    },
                ))
            }
        }

        Ok(result)
    }
}
