// Author: Dustin Pilgrim
// License: MIT

use crate::daemon::Daemon;
use eventline::journal::rotation::LogPolicy;
use eventline::runtime::enable_file_output_rotating;
use eventline::runtime::run_header::RunHeader;
use std::io;
use std::path::PathBuf;

use crate::cli::Args;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

pub async fn run(args: Args) -> Result<(), AnyError> {
    // eventline (init early so warn logs are consistent)
    eventline::runtime::init().await;

    if args.verbose {
        eventline::runtime::enable_console_output(true);
        eventline::runtime::set_log_level(eventline::runtime::LogLevel::Debug);
        eventline::debug!("debug logging enabled");
    } else {
        eventline::runtime::enable_console_output(false);
        eventline::runtime::set_log_level(eventline::runtime::LogLevel::Info);
    }

    // ------------------------------
    // single-instance
    // ------------------------------
    let _instance_lock = match crate::app::platform::acquire_single_instance_lock() {
        Ok(l) => l,

        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            eventline::warn!("{e}");

            if !args.verbose {
                eprintln!("Error: {e}");
            }

            return Ok(()); // clean exit
        }

        Err(e) => {
            eventline::error!("failed to acquire instance lock: {e}");
            return Err(Box::new(e));
        }
    };

    // ------------------------------
    // wayland sanity
    // ------------------------------
    if let Err(e) = crate::app::platform::ensure_wayland_alive() {
        eventline::warn!("not running in wayland session: {e}");

        if !args.verbose {
            eprintln!("Error: not running in wayland session: {e}");
        }

        return Ok(()); // clean exit
    }

    // file logging
    if let Some(path) = crate::app::platform::default_log_path() {
        let header = RunHeader::new("stasis daemon run start");
        let policy = LogPolicy::default();

        if let Err(e) = enable_file_output_rotating(&path, policy, Some(header)) {
            eventline::error!("failed to enable file logging: {}", e);
        } else {
            eventline::info!("file logging enabled: {}", path.display());
        }
    }

    eventline::info!("stasis starting");

    // resolve config path
    let mut config_path: PathBuf = match args.config.as_deref() {
        Some(p) => p.to_path_buf(),
        None => crate::config::resolve_default_config_path(),
    };

    if args.config.is_none() {
        if let Err(e) = crate::config::bootstrap::ensure_user_config_exists() {
            eventline::warn!("failed to bootstrap default config: {e}");
        }

        config_path = crate::config::resolve_default_config_path();
    }

    if config_path.exists() {
        match crate::config::migrate::migrate_in_place(&config_path) {
            Ok(crate::config::migrate::MigrateOutcome::Migrated { backup_path }) => {
                eventline::info!(
                    "migrated old config to new format; backup at {}",
                    backup_path.display()
                );
            }
            Ok(crate::config::migrate::MigrateOutcome::NotOldFormat) => {}
            Err(e) => {
                eventline::error!("config migration failed: {e}");
            }
        }
    }

    let cfg_file = crate::config::load_from_path(&config_path).map_err(|e| {
        eventline::error!("{e}");
        e
    })?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    crate::app::platform::spawn_wayland_socket_watcher(shutdown_tx.clone());

    let mut daemon = Daemon::new(cfg_file.cfg, config_path);

    let mut daemon_task = tokio::spawn({
        let shutdown_tx = shutdown_tx.clone();
        async move { daemon.run(shutdown_rx, shutdown_tx).await }
    });

    tokio::select! {
        res = &mut daemon_task => {
            match res {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(join_err) => Err(Box::new(join_err) as AnyError),
            }?;
            Ok(())
        }

        _ = tokio::signal::ctrl_c() => {
            eventline::info!("received Ctrl+C, shutting down");
            let _ = shutdown_tx.send(true);

            match daemon_task.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(join_err) => Err(Box::new(join_err)),
            }
        }
    }
}
