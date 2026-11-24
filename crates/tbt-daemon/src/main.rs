use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, ValueEnum};
use tbt_collector::{Collector, CollectorConfig, RunOutcome, RunTrigger, scheduler};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

const LOG_DIR: &str = "logs";
const LOG_PREFIX: &str = "tbt-daemon";
const MAX_LOG_FILES: usize = 30;

#[derive(Debug, Parser)]
#[command(
    name = "tbt-daemon",
    version,
    about = "Headless TokenBurnTrack collector"
)]
struct Cli {
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
    let config = match &cli.config {
        Some(path) => CollectorConfig::load(path)?,
        None => CollectorConfig::load_default()?,
    };
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
    let collector = Collector::builtin(data_dir, config);
    if cli.once {
        let trigger = cli.trigger.map_or(RunTrigger::Hourly, RunTrigger::from);
        return run_once(&collector, trigger);
    }

    let first = cli.trigger.map_or(RunTrigger::Startup, RunTrigger::from);
    let (_stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
    scheduler::run_loop(interval, first, &stop_rx, |trigger| {
        if let Err(e) = run_once(&collector, trigger) {
            tracing::error!("ingest run failed: {e:#}");
            eprintln!("tbt-daemon: {e:#}");
        }
    });
    Ok(())
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
