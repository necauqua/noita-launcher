use clap::Parser;
use color_eyre::Section;
use eyre::OptionExt;
use inquire::InquireError;
use noita_launcher::{error::UserError, launcher::NoitaLauncher, printer::Printer};

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
    subcommand: Option<Subcommand>,
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
    #[clap(short, long)]
    force: bool,
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
    /// Names of the instances to delete
    #[clap(required = true)]
    names: Vec<String>,
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

async fn dispatch_cli(subcommand: Subcommand, mut launcher: NoitaLauncher) -> eyre::Result<()> {
    match subcommand {
        Subcommand::Login(args) => {
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
            launcher.login(username.trim(), password.trim()).await?
        }
        Subcommand::Logout => launcher.logout().await?,
        Subcommand::Run(args) => {
            launcher
                .run_instance(&args.instance, &args.save, args.force)
                .await?
        }
        Subcommand::New(args) => {
            launcher
                .new_instance(
                    &args.name,
                    args.fetch.manifest,
                    args.fetch.branch.as_deref(),
                    args.fetch.validate,
                )
                .await?;
        }
        Subcommand::Prefetch(args) => {
            launcher
                .prefetch(args.manifest, args.branch.as_deref(), args.validate)
                .await?
        }
        Subcommand::PrefetchAll(args) => launcher.prefetch_all(args.validate).await?,
        Subcommand::Remove(args) => launcher.remove_instances(&args.names).await?,
        Subcommand::List => launcher.list_instances().await?,
        Subcommand::Saves => launcher.list_saves().await?,
    }
    Ok(())
}

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

    tokio::fs::write(
        &trampoline,
        include_bytes!(concat!(env!("OUT_DIR"), "/noita-trampoline.exe")),
    )
    .await?;
    tokio::fs::write(
        &hook_dll,
        include_bytes!(concat!(env!("OUT_DIR"), "/noita-path-hook.dll")),
    )
    .await?;

    let args = Args::parse();

    let Some(subcommand) = args.subcommand else {
        println!("your \x1b[9mad\x1b[m gui here");
        return Ok(());
    };

    let printer = Printer::new(args.quiet);
    let launcher = NoitaLauncher::new(app_dir, trampoline, hook_dll, cache_dir, printer.clone());

    match dispatch_cli(subcommand, launcher).await {
        Ok(()) => Ok(()),
        Err(e) => match e.downcast::<UserError>() {
            Ok(user_error) => {
                printer.error(format!("Error: {user_error}"));
                if let Some(hint) = user_error.hint {
                    printer.hint(format!("\n\nHint: {hint}\n"));
                }
                std::process::exit(1);
            }
            Err(e) => Err(e),
        },
    }
}
