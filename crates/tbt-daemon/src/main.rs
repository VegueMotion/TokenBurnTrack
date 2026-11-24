use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tbt_collector::{Collector, CollectorConfig, RunOutcome, RunTrigger, scheduler};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

mod install;
mod templates;

const LOG_DIR: &str = "logs";
const LOG_PREFIX: &str = "tbt-daemon";
const MAX_LOG_FILES: usize = 30;

#[derive(Debug, Parser)]
#[command(
    name = "tbt-daemon",
    version,
    about = "Headless TokenBurnTrack collector",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(
        long,
        help = "Run one ingest and exit (for systemd timers, launchd, Task Scheduler)"
    )]
    once: bool,
    #[arg(
        long,
        value_name = "DIR",
        help = "Data directory; overrides TBT_DATA_DIR and the config file"
    )]
    data_dir: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Config file [default: <config dir>/tokenburntrack/config.toml]"
    )]
    config: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        help = "Trigger recorded for the first run [default: hourly with --once, startup otherwise]"
    )]
    trigger: Option<Trigger>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(
        about = "Install the hourly schedule (systemd user timer, launchd agent or scheduled task)"
    )]
    Install(InstallArgs),
    #[command(about = "Remove the hourly schedule and the files written by install")]
    Uninstall(UninstallArgs),
}

#[derive(Debug, Args)]
struct InstallArgs {
    #[arg(long, help = "Print the files and commands without changing anything")]
    dry_run: bool,
    #[arg(
        long,
        value_name = "PATH",
        help = "Daemon binary the schedule runs [default: this executable]"
    )]
    bin: Option<PathBuf>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Also install a desktop entry and icon for this tbt-app binary (Linux)"
    )]
    app: Option<PathBuf>,
    #[arg(long, help = "Write the files but do not enable or start the schedule")]
    no_enable: bool,
    #[arg(
        long,
        value_name = "DIR",
        help = "Data directory the scheduled daemon uses; passed to it as --data-dir"
    )]
    data_dir: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Config file the scheduled daemon uses; passed to it as --config"
    )]
    config: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct UninstallArgs {
    #[arg(long, help = "Print the files and commands without changing anything")]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Trigger {
    Hourly,
    Startup,
    Manual,
    Watch,
}

impl From<Trigger> for RunTrigger {
    fn from(t: Trigger) -> RunTrigger {
        match t {
            Trigger::Hourly => RunTrigger::Hourly,
            Trigger::Startup => RunTrigger::Startup,
            Trigger::Manual => RunTrigger::Manual,
            Trigger::Watch => RunTrigger::Watch,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            eprintln!("tbt-daemon: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match &cli.command {
        Some(Command::Install(args)) => return install(args),
        Some(Command::Uninstall(args)) => return uninstall(args),
        None => {}
    }
    let config = load_config(cli.config.as_deref())?;
    let data_dir = cli
        .data_dir
        .clone()
        .or_else(|| config.resolve_data_dir())
        .context("no data directory: pass --data-dir or set TBT_DATA_DIR")?;
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("cannot create data directory {}", data_dir.display()))?;
    let _log_guard = init_logging(&data_dir)?;
    tracing::info!(data_dir = %data_dir.display(), once = cli.once, "tbt-daemon starting");

    let interval = config.sync_interval_minutes;
    let mut loaded = config.clone();
    let mut collector = Collector::builtin(data_dir.clone(), config);
    if cli.once {
        let trigger = cli.trigger.map_or(RunTrigger::Hourly, RunTrigger::from);
        return run_once(&collector, trigger);
    }

    let first = cli.trigger.map_or(RunTrigger::Startup, RunTrigger::from);
    let (_stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
    scheduler::run_loop(interval, first, &stop_rx, |trigger| {
        match load_config(cli.config.as_deref()) {
            Ok(fresh) if fresh != loaded => {
                tracing::info!("config changed; reloading");
                loaded = fresh.clone();
                collector = Collector::builtin(data_dir.clone(), fresh);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("config reload failed, keeping the previous one: {e:#}"),
        }
        if let Err(e) = run_once(&collector, trigger) {
            tracing::error!("ingest run failed: {e:#}");
            eprintln!("tbt-daemon: {e:#}");
        }
    });
    Ok(())
}

fn load_config(path: Option<&Path>) -> anyhow::Result<CollectorConfig> {
    Ok(match path {
        Some(path) => CollectorConfig::load(path)?,
        None => CollectorConfig::load_default()?,
    })
}

fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("invalid path {}", path.display()))
}

fn install(args: &InstallArgs) -> anyhow::Result<()> {
    let config = load_config(args.config.as_deref())?;
    let data_dir = match &args.data_dir {
        Some(dir) => dir.clone(),
        None => config
            .resolve_data_dir()
            .context("no data directory: pass --data-dir or set TBT_DATA_DIR")?,
    };
    let data_dir = absolute(&data_dir)?;
    let data_dir_from_env =
        std::env::var_os(tbt_store::paths::DATA_DIR_ENV).is_some_and(|v| !v.is_empty());
    let mut daemon_args = Vec::new();
    if args.data_dir.is_some() || data_dir_from_env {
        daemon_args.push("--data-dir".to_owned());
        daemon_args.push(data_dir.display().to_string());
    }
    if let Some(config) = &args.config {
        daemon_args.push("--config".to_owned());
        daemon_args.push(absolute(config)?.display().to_string());
    }
    let bin = match &args.bin {
        Some(bin) => absolute(bin)?,
        None => std::env::current_exe().context("cannot locate the tbt-daemon executable")?,
    };
    let app = args.app.as_deref().map(absolute).transpose()?;
    if !args.dry_run {
        for path in std::iter::once(&bin).chain(&app) {
            if !path.is_file() {
                bail!("{} is not a file", path.display());
            }
        }
    }
    let target = install::Target::detect()?;
    if app.is_some() && target.platform != install::Platform::Systemd {
        eprintln!("tbt-daemon: --app is only used on Linux; ignoring it");
    }
    let options = install::InstallOptions {
        bin,
        app,
        data_dir,
        daemon_args,
        enable: !args.no_enable,
    };
    let plan = install::install_plan(&target, &options)?;
    install::execute(
        &plan,
        args.dry_run,
        &mut install::SystemRunner,
        &mut std::io::stdout(),
    )
}

fn uninstall(args: &UninstallArgs) -> anyhow::Result<()> {
    let target = install::Target::detect()?;
    install::execute(
        &install::uninstall_plan(&target),
        args.dry_run,
        &mut install::SystemRunner,
        &mut std::io::stdout(),
    )
}

fn run_once(collector: &Collector, trigger: RunTrigger) -> anyhow::Result<()> {
    match collector.ingest_run(trigger)? {
        RunOutcome::Busy => {
            let msg = format!(
                "collector busy: another process holds {}; run skipped",
                tbt_collector::lock::lock_path(collector.data_dir()).display()
            );
            tracing::info!("{msg}");
            println!("{msg}");
        }
        RunOutcome::Finished(report) => {
            for e in &report.errors {
                tracing::warn!("{e}");
            }
            println!("{}", report.summary());
        }
    }
    Ok(())
}

fn init_logging(data_dir: &Path) -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    let log_dir = data_dir.join(LOG_DIR);
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("cannot create log directory {}", log_dir.display()))?;
    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(&log_dir)
        .context("cannot open the log file")?;
    let (file_writer, guard) = tracing_appender::non_blocking(appender);
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(filter()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer)
                .with_filter(filter()),
        )
        .try_init()
        .context("cannot initialise logging")?;
    Ok(guard)
}
