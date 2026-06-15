use std::fmt::Write as _;

use clap::ColorChoice;
use clap::Parser;
use color_eyre::Section;
use eyre::OptionExt;
use inquire::InquireError;
use noita_launcher::error::UserError;
use noita_launcher::launcher::NoitaLauncher;
use noita_launcher::printer::Printer;
use yansi::Condition;
use yansi::Paint;

/// Manage multiple isolated Noita instances, each pinned to a specific game version.
///
/// Downloads game files directly from Steam depots without requiring the Steam client.
/// Credentials are stored in the OS keyring. On Linux, instances are run via umu-run.
#[derive(clap::Parser)]
struct Args {
    /// Suppress all non-essential output
    #[clap(short, long)]
    quiet: bool,
    /// Specify WHEN to colorize output
    #[clap(
        short = 'C',
        long,
        default_value = "auto",
        default_missing_value = "always",
        value_name = "WHEN",
        num_args = 0..=1,
        require_equals = true,
    )]
    color: ColorChoice,
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

/// Run a Noita instance. Creates the default instance (called `main`) first if it does not exist.
#[derive(clap::Parser)]
struct RunArgs {
    /// Instance to run; defaults to `main`
    #[clap(default_value = "main")]
    instance: String,
    /// Version to use to run the instance. Without `--force`, only when creating new instances.
    version: Option<String>,
    /// Override the version association of an existing instance.
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

/// Delete a version.
#[derive(clap::Parser)]
struct RemoveVersionArgs {
    /// Names of the versions to delete
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
    RemoveVersion(RemoveVersionArgs),
    /// List all instances
    #[clap(visible_alias = "ls")]
    List,
    /// List all set up versions
    Versions,
}

async fn dispatch_cli(
    subcommand: Subcommand,
    printer: Printer,
    mut launcher: NoitaLauncher,
) -> eyre::Result<()> {
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
                .run_instance(&args.instance, args.version.as_deref(), args.force)
                .await?
        }
        Subcommand::New(args) => {
            launcher
                .new_version(
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
        Subcommand::RemoveVersion(args) => launcher.remove_versions(&args.names).await?,
        Subcommand::Versions => {
            let mut versions = launcher.list_versions().await?;

            if versions.is_empty() {
                printer.hint("No versions found, run `noita new` to create one");
                return Ok(());
            }

            let max_name_len = versions
                .iter()
                .map(|(n, _)| n.len())
                .max()
                .unwrap_or_default();

            versions.sort_by_key(|(_, meta)| meta.order);

            for (name, meta) in versions {
                println!(
                    "{:width$} {} ({})",
                    name.bold(),
                    meta.human_timestamp().dim(),
                    format!("{:x}", meta.pe_timestamp).green(),
                    width = max_name_len,
                );
            }
        }
        Subcommand::List => {
            let mut instances = launcher.list_instances().await?;

            if instances.is_empty() {
                printer.hint("No instances found, run an instance (with `noita run`)");
                return Ok(());
            }

            let max_name_len = instances
                .iter()
                .map(|(n, _)| n.len())
                .max()
                .unwrap_or_default();

            instances.sort_by_key(|(_, meta)| meta.order);

            for (name, meta) in instances {
                let s = match meta.stats {
                    None => Default::default(),
                    Some(stats) => {
                        let mut s = String::new();
                        s += " | ";
                        if stats.wins > 0 {
                            write!(&mut s, "wins: {} ", stats.wins).unwrap();
                        }
                        if stats.deaths > 0 {
                            write!(&mut s, "deaths: {} ", stats.deaths).unwrap();
                        }
                        if stats.win_streak > 0 || stats.win_streak_pb > 0 {
                            write!(
                                &mut s,
                                "streak: {} (pb: {})",
                                stats.win_streak, stats.win_streak_pb
                            )
                            .unwrap();
                        }
                        s.truncate(s.trim_end().len());
                        s
                    }
                };

                println!(
                    "{:width$} ({}){s}",
                    name.bold(),
                    meta.version.dim(),
                    width = max_name_len,
                );
            }
        }
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
    yansi::whenever(match args.color {
        ColorChoice::Auto => Condition::TTY_AND_COLOR,
        ColorChoice::Always => Condition::ALWAYS,
        ColorChoice::Never => Condition::NEVER,
    });

    let Some(subcommand) = args.subcommand else {
        println!("your {} gui here", "ad".strike());
        return Ok(());
    };

    let printer = Printer::new(args.quiet);
    let launcher = NoitaLauncher::new(app_dir, trampoline, hook_dll, cache_dir, printer.clone());

    match dispatch_cli(subcommand, printer.clone(), launcher).await {
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
