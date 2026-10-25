//! Logging setup.
//!
//! Debug builds mirror log records to `dev_data/logs/peerdav.log` in addition
//! to the terminal so `pnpm tauri dev` runs keep their full log history in the
//! project workspace. Release builds keep the previous stderr-only behavior.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{Log, Metadata, Record};

use crate::paths;

const LOG_FILE_NAME: &str = "peerdav.log";

struct TeeLogger {
    console: env_logger::Logger,
    file: Mutex<File>,
}

impl Log for TeeLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.console.enabled(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        if !self.console.enabled(record.metadata()) {
            return;
        }

        self.console.log(record);

        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(
                file,
                "[{millis}] {:<5} {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
            let _ = file.flush();
        }
    }

    fn flush(&self) {
        self.console.flush();
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

/// Initializes logging, mirroring output to `dev_data/logs/peerdav.log` when a
/// `dev_data` directory is available.
pub fn init() {
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));

    let Some(log_dir) = paths::dev_data_subdir("logs") else {
        init_console_only(builder);
        return;
    };

    let log_path = log_dir.join(LOG_FILE_NAME);
    let file = fs::create_dir_all(&log_dir)
        .and_then(|()| OpenOptions::new().create(true).append(true).open(&log_path));
    let file = match file {
        Ok(file) => file,
        Err(error) => {
            eprintln!(
                "failed to open dev log file {}: {error}",
                log_path.display()
            );
            init_console_only(builder);
            return;
        }
    };

    let logger = TeeLogger {
        console: builder.build(),
        file: Mutex::new(file),
    };
    if let Err(error) = log::set_boxed_logger(Box::new(logger)) {
        eprintln!("failed to initialize logging: {error}");
        return;
    }
    log::set_max_level(log::LevelFilter::Trace);
}

fn init_console_only(mut builder: env_logger::Builder) {
    if let Err(error) = builder.try_init() {
        eprintln!("failed to initialize logging: {error}");
    }
}
