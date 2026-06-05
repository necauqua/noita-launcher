use std::io::ErrorKind;
use std::path::PathBuf;

use clap::Parser;
use color_eyre::Section;
use eyre::Context;
use eyre::OptionExt;
use eyre::bail;
use eyre::eyre;
use indicatif::ProgressStyle;
use inquire::InquireError;
use keyring_core::Entry;
use noita_launcher::download::InstanceDownloader;
use noita_launcher::meta;
use noita_launcher::steam::AccessDenied;
use noita_launcher::steam::Steam;
use owo_colors::OwoColorize as _;
use owo_colors::Stream;
use reqwest::Client;
use steam_vent::Connection;
use steam_vent::ConnectionError;
use steam_vent::ServerList;
use steam_vent::auth::*;
use tokio::process::Command;

const APP_ID: u32 = 881100;
const DEPOT_ID: u32 = 881101;

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

#[derive(Clone)]
struct Printer(indicatif::MultiProgress);

impl Printer {
    fn new(quiet: bool) -> Self {
        let p = indicatif::MultiProgress::new();
        if quiet {
            p.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        Self(p)
    }

    fn hint(&self, msg: impl Into<String>) {
        let colored = msg
            .into()
            .if_supports_color(Stream::Stderr, |t| t.dimmed())
            .to_string();
        self.0.println(colored).unwrap();
    }

    fn bar(&mut self, len: u64) -> indicatif::ProgressBar {
        let bar = indicatif::ProgressBar::new(len).with_style(
            ProgressStyle::with_template(
                "{prefix:.dim} {spinner:.green} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-"),
        );
        self.0.add(bar.clone());
        bar
    }
}

struct NoitaLauncher {
    app_dir: PathBuf,
    trampoline: PathBuf,
    hook_dll: PathBuf,
    cache_dir: PathBuf,
    printer: Printer,
    steam: Option<Steam>,
    http: Option<reqwest::Client>,
}

impl NoitaLauncher {
    fn new(
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
            _ => self.http.insert(Client::new()).clone(),
        }
    }

    async fn login(&mut self, args: LoginArgs) -> eyre::Result<()> {
        let entry = Self::keyring_entry()?;

        let username = match args.username {
            Some(u) => u,
            None => match inquire::Text::new("Steam username:").prompt() {
                Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
                    return Ok(());
                }
                u => u?,
            },
        };

        let password = if args.stdin_password {
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
            buf
        } else {
            match inquire::Password::new("Steam password:").prompt() {
                Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
                    return Ok(());
                }
                u => u?,
            }
        };

        let conn = match Connection::login(
            &ServerList::discover().await?,
            username.trim(),
            password.trim(),
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

    async fn logout(&mut self) -> eyre::Result<()> {
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

    async fn run_instance(&mut self, args: RunArgs) -> eyre::Result<()> {
        let instance_dir = self.app_dir.join("instances").join(&args.instance);

        if !tokio::fs::try_exists(&instance_dir).await? {
            if args.instance != "main" {
                return Err(eyre!("instance '{}' does not exist", args.instance)
                    .suggestion("run `noita new` first"));
            }
            self.printer
                .hint("Default instance 'main' does not exist, setting it up..");
            self.new_instance(NewArgs {
                name: args.instance,
                fetch: FetchArgs::default(),
            })
            .await?;
        }
        let save_path = self.app_dir.join("saves").join(&args.save);
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

    async fn setup_downloader(
        &mut self,
        args: FetchArgs,
    ) -> eyre::Result<(u64, InstanceDownloader)> {
        let steam = self.steam().await?;

        let manifest = match args.manifest {
            Some(manifest) => manifest,
            None => {
                let current_release = steam.get_current_release(APP_ID, DEPOT_ID).await?;
                let branch = args.branch.as_deref().unwrap_or("public");
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

        let (meta, payload) = match steam
            .fetch_manifest(APP_ID, &depot, manifest, args.branch)
            .await
        {
            Err(e) if e.is::<AccessDenied>() => {
                bail!("manifest {manifest} was not publicly available")
            }
            m => m?,
        };

        let chunk_cache = self.cache_dir.join("chunks");
        tokio::fs::create_dir_all(&chunk_cache).await?;

        // eh we track uncompressed download size because cache's uncompressed
        let size = meta.cb_disk_original();

        Ok((
            size,
            InstanceDownloader::new(depot, payload, cdn_hosts, chunk_cache, self.http(), 32),
        ))
    }

    async fn prefetch(&mut self, args: FetchArgs) -> eyre::Result<()> {
        let validate = args.validate;
        let (size, downloader) = self.setup_downloader(args).await?;

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

    async fn prefetch_all(&mut self, args: FetchAllArgs) -> eyre::Result<()> {
        let versions = meta::read().await?;

        let len = versions.len();
        for (i, version) in versions.into_iter().enumerate() {
            self.printer.hint(format!(
                "[{}/{len}] Prefetching version '{}'",
                i + 1,
                version.title,
            ));
            self.prefetch(FetchArgs {
                manifest: Some(version.manifest),
                branch: version.branch,
                validate: args.validate,
            })
            .await?;
        }

        Ok(())
    }

    async fn new_instance(&mut self, args: NewArgs) -> eyre::Result<()> {
        let instances_path = self.app_dir.join("instances");
        tokio::fs::create_dir_all(&instances_path).await?;

        let instance_path = instances_path.join(&args.name);
        if tokio::fs::try_exists(&instance_path).await? {
            bail!("instance '{}' already exists", args.name);
        }

        let validate = args.fetch.validate;
        let (size, downloader) = self.setup_downloader(args.fetch).await?;

        let cache_bar = self.printer.bar(size).with_prefix("Checking cached chunks");
        let cached_size = downloader.compute_cached_size(validate, cache_bar).await?;

        let download_size = size - cached_size;

        if download_size == 0 {
            self.printer
                .hint("All chunks already cached, no download needed");
        }

        let temp_path = self.app_dir.join("temp").join(format!(
            "{}.{}",
            args.name,
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
            bail!(
                "instance '{}' was created during the download, aborting",
                args.name
            );
        }
        tokio::fs::rename(temp_path, instance_path).await?;

        Ok(())
    }

    async fn remove_instance(&mut self, args: RemoveArgs) -> eyre::Result<()> {
        let instance_dir = self.app_dir.join("instances").join(&args.name);
        if !tokio::fs::try_exists(&instance_dir).await? {
            bail!("instance '{}' does not exist", args.name);
        }

        tokio::fs::remove_dir_all(instance_dir).await?;

        Ok(())
    }

    // todo: list some stats, like number of installed mods etc
    // ( + manifest id mb? get noita build string from noita.exe, or some _version_hash.txt matching bs)
    async fn list_instances(&self) -> eyre::Result<()> {
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
    async fn list_saves(&self) -> eyre::Result<()> {
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
//     instances/<name>/.. - instance dir, cwd for a particular Noita version
//     saves/<name>/.. - a single save dir

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();
    keyring::use_native_store(true)?;

    let app_dir = dirs::data_dir()
        .ok_or_eyre("No user data directory ($XDG_DATA_HOME or %appdata%\\Roaming) defined")
        .note("This should not happen, your OS setup is very unconventional")?
        .join("noita-launcher");

    let cache_dir = dirs::cache_dir()
        .ok_or_eyre("No user cache directory ($XDG_CACHE_HOME or %appdata%\\Local) defined")
        .note("This should not happen, your OS setup is very unconventional")?
        .join("noita-launcher");

    let trampoline = app_dir.join("noita-trampoline.exe");
    let hook_dll = app_dir.join("noita-path-hook.dll");

    if !trampoline.exists() {
        tokio::fs::write(
            &trampoline,
            include_bytes!(concat!(env!("OUT_DIR"), "/noita-trampoline.exe")),
        )
        .await?;
    }
    if !hook_dll.exists() {
        tokio::fs::write(
            &hook_dll,
            include_bytes!(concat!(env!("OUT_DIR"), "/noita-path-hook.dll")),
        )
        .await?;
    }

    let args = Args::parse();
    let printer = Printer::new(args.quiet);
    let mut app = NoitaLauncher::new(app_dir, trampoline, hook_dll, cache_dir, printer);

    match args.subcommand {
        Subcommand::Login(args) => app.login(args).await?,
        Subcommand::Logout => app.logout().await?,
        Subcommand::Run(args) => app.run_instance(args).await?,
        Subcommand::New(args) => app.new_instance(args).await?,
        Subcommand::Prefetch(args) => app.prefetch(args).await?,
        Subcommand::PrefetchAll(args) => app.prefetch_all(args).await?,
        Subcommand::Remove(args) => app.remove_instance(args).await?,
        Subcommand::List => app.list_instances().await?,
        Subcommand::Saves => app.list_saves().await?,
    }

    Ok(())
}

/// Manage multiple isolated Noita instances, each pinned to a specific game version.
///
/// Downloads game files directly from Steam depots without requiring the Steam client.
/// Credentials are stored in the OS keyring. On Linux, instances are run via umu-run.
#[derive(clap::Parser)]
struct Args {
    /// Suppress all non-essential output
    #[clap(short, long)]
    quiet: bool,
    #[clap(subcommand)]
    subcommand: Subcommand,
}

/// Log in with a Steam account. Credentials are stored in the OS keyring.
#[derive(clap::Parser)]
struct LoginArgs {
    /// Steam username; prompted interactively if not provided
    #[clap(short, long)]
    username: Option<String>,
    /// Read password from stdin instead of prompting
    #[clap(short = 's', long)]
    stdin_password: bool,
}

/// Run a Noita instance. Creates the default instance first if it does not exist.
#[derive(clap::Parser)]
struct RunArgs {
    /// Instance name to run
    #[clap(default_value = "main")]
    instance: String,
    /// Save slot to use
    #[clap(default_value = "main")]
    save: String,
}

/// Create a new instance by downloading a specific Noita version from Steam depots.
#[derive(clap::Parser)]
struct NewArgs {
    /// Name for the new instance
    #[clap(default_value = "main")]
    name: String,
    #[clap(flatten)]
    fetch: FetchArgs,
}

/// Download depot chunks for a specific version without creating an instance.
/// Useful for pre-warming the cache before running `new`.
#[derive(clap::Parser, Default)]
struct FetchArgs {
    /// Depot manifest ID to download; defaults to the latest on the branch
    #[clap(short, long)]
    manifest: Option<u64>,
    /// Steam branch to resolve the latest manifest from (e.g. public, noitabeta)
    #[clap(short, long)]
    branch: Option<String>,
    /// Recheck SHA-1 of already-cached chunks and re-download any that are corrupt
    #[clap(long)]
    validate: bool,
}

/// Pre-fetch depot chunks for all known Noita versions.
#[derive(clap::Parser, Default)]
struct FetchAllArgs {
    /// Recheck SHA-1 of already-cached chunks — CPU-heavy when run across all versions
    #[clap(long)]
    validate: bool,
}

/// Delete an instance directory.
#[derive(clap::Parser)]
struct RemoveArgs {
    /// Name of the instance to delete
    name: String,
}

#[derive(clap::Subcommand)]
enum Subcommand {
    /// Log in with a Steam account (credentials stored in the OS keyring)
    Login(LoginArgs),
    /// Remove stored Steam credentials from the OS keyring
    Logout,
    /// Run a Noita instance, optionally specifying an instance and save slot
    Run(RunArgs),
    /// Create a new instance by downloading a Noita version from Steam depots
    New(NewArgs),
    /// Download depot chunks without creating an instance (pre-warm the cache)
    Prefetch(FetchArgs),
    /// Pre-fetch depot chunks for all known Noita versions
    PrefetchAll(FetchAllArgs),
    /// Delete an instance directory
    #[clap(visible_alias = "rm")]
    Remove(RemoveArgs),
    /// List all installed instances
    #[clap(visible_alias = "ls")]
    List,
    /// List all save slots
    Saves,
}
