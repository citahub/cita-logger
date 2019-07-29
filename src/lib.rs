// Copyright 2016-2019 Cryptape Technologies LLC.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0>.
// This file may not be copied, modified, or distributed
// except according to those terms

pub use log::{debug, error, info, log, log_enabled, trace, warn};

use chrono::Local;
use crossbeam_channel::{bounded, Receiver};
use libc::c_int;
use log::LevelFilter;
use log4rs::append::console::ConsoleAppender;
use log4rs::append::file::FileAppender;
use log4rs::config::{Appender, Config, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;
use std::env;
use std::fs;
use std::io::Error;
use std::str::FromStr;
use std::sync::{Once, ONCE_INIT};
use std::thread;
use std::vec::Vec;

pub enum LogFavour<'a> {
    Stdout(&'a str),
    File(&'a str),
}

#[derive(Debug, Clone)]
struct Directive {
    // Module name
    name: String,
    // Log level
    level: LevelFilter,
}

static INIT_LOG: Once = ONCE_INIT;

fn notify(signals: &[c_int]) -> Result<Receiver<c_int>, Error> {
    let (s, r) = bounded(100);
    let signals = signal_hook::iterator::Signals::new(signals)?;
    thread::spawn(move || {
        for signal in signals.forever() {
            let _ = s.send(signal);
        }
    });
    Ok(r)
}

pub fn init_config(favour: &LogFavour) {
    INIT_LOG.call_once(|| {
        // Parse RUST_LOG
        let directives: Vec<Directive> = match env::var("RUST_LOG") {
            Ok(s) => parse_env(&s),
            Err(_) => Vec::new(),
        };

        match favour {
            LogFavour::Stdout(service_name) => {
                let config = config_console_appender(service_name, directives);
                log4rs::init_config(config).unwrap();
            }
            LogFavour::File(service_name) => {
                // The config of log4rs
                let log_name = format!("logs/{}.log", service_name);
                let directives_clone = directives.clone();
                let config = config_file_appender(&log_name, directives_clone);
                let handle = log4rs::init_config(config).unwrap();

                // Log rotate via signal(USR1)
                let signal = notify(&[signal_hook::SIGUSR1]).unwrap();

                // Any and all threads spawned must come after the first call to notify (or notify_on).
                // This is so all spawned threads inherit the blocked status of signals.
                // If a thread starts before notify is called, it will not have the correct signal mask.
                // When a signal is delivered, the result is indeterminate.
                let service_name_clone = service_name.to_string();
                thread::spawn(move || {
                    loop {
                        // Blocks until this process is sent an USR1 signal.
                        signal.recv().unwrap();

                        // Rotate current log file
                        let time_stamp = Local::now().format("_%Y-%m-%d_%H-%M-%S");
                        let log_rotate_name =
                            format!("logs/{}{}.log", &service_name_clone, time_stamp);
                        if let Err(e) = fs::rename(&log_name, log_rotate_name) {
                            warn!("logrotate failed because of {:?}", e.kind());
                            continue;
                        }

                        // Reconfig
                        let directives_clone = directives.clone();
                        let new_config = config_file_appender(&log_name, directives_clone);
                        handle.set_config(new_config);
                    }
                });
            }
        }
    });
}

// Used in tests
pub fn init() {
    init_config(&LogFavour::Stdout(""));
}

// Used in unit case
pub fn silent() {
    INIT_LOG.call_once(|| {
        let config = Config::builder()
            .build(Root::builder().build(LevelFilter::Off))
            .unwrap();
        log4rs::init_config(config).unwrap();
    });
}

// Simple parse env (e.g: crate1,crate2::mod=debug,crate3::mod=trace)
fn parse_env(env: &str) -> Vec<Directive> {
    let mut directives = Vec::new();

    for s in env.split(',') {
        if s.is_empty() {
            continue;
        }
        let mut parts = s.split('=');
        let (log_level, name) = match (parts.next(), parts.next().map(str::trim), parts.next()) {
            (Some(part0), None, None) => match LevelFilter::from_str(part0) {
                Ok(num) => {
                    println!(
                        "warning: log level '{}' need explicit crate or module name.",
                        num
                    );
                    continue;
                }
                Err(_) => (LevelFilter::Info, part0),
            },
            (Some(part0), Some(""), None) => (LevelFilter::Info, part0),
            (Some(part0), Some(part1), None) => match LevelFilter::from_str(part1) {
                Ok(num) => (num, part0),
                _ => {
                    println!(
                        "warning: invalid logging spec '{}', \
                         ignoring it",
                        part1
                    );
                    continue;
                }
            },
            _ => {
                println!(
                    "warning: invalid logging spec '{}', \
                     ignoring it",
                    s
                );
                continue;
            }
        };

        if !name.is_empty() {
            directives.push(Directive {
                name: name.to_string(),
                level: log_level,
            });
        }
    }

    directives
}

fn create_loggers(directives: Vec<Directive>, appender: &str) -> Vec<Logger> {
    let mut loggers = Vec::new();

    if directives.is_empty() {
        return loggers;
    }

    // Create loggers via module/crate and log level
    for directive in directives {
        let appender_clone = appender.to_string();
        let logger = Logger::builder()
            .appender(appender_clone)
            .additive(false)
            .build(directive.name, directive.level);
        loggers.push(logger);
    }

    loggers
}

// FileAppender config
fn config_file_appender(file_path: &str, directives: Vec<Directive>) -> Config {
    let requests = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(
            "{d(%Y-%m-%d - %H:%M:%S)} | {t:20.20} - {L:5} | {l:5} - {m}{n}",
        )))
        .build(file_path)
        .unwrap();

    let mut config_builder =
        Config::builder().appender(Appender::builder().build("requests", Box::new(requests)));

    let loggers = create_loggers(directives, "requests");

    // Config crate or module log level
    if !loggers.is_empty() {
        config_builder = config_builder.loggers(loggers.into_iter());
    }

    // Config global log level
    config_builder
        .build(
            Root::builder()
                .appender("requests")
                .build(LevelFilter::Info),
        )
        .unwrap()
}

// ConsoleAppender config
fn config_console_appender(service_name: &str, directives: Vec<Directive>) -> Config {
    let pattern = format!("[{}]: ", service_name) + "{d} - {l} - {m}{n}";
    let stdout = ConsoleAppender::builder()
        .encoder(Box::new(PatternEncoder::new(&pattern)))
        .build();

    let mut config_builder =
        Config::builder().appender(Appender::builder().build("stdout", Box::new(stdout)));

    let loggers = create_loggers(directives, "stdout");

    // Config crate or module log level
    if !loggers.is_empty() {
        config_builder = config_builder.loggers(loggers.into_iter());
    }

    // Config global log level
    config_builder
        .build(Root::builder().appender("stdout").build(LevelFilter::Info))
        .unwrap()
}

#[cfg(test)]
mod tests {

    use super::parse_env;
    use log::LevelFilter;

    #[test]
    fn parse_env_valid() {
        let directives = parse_env("crate1::mod1,crate1::mod2=debug,crate2=trace");
        assert_eq!(directives.len(), 3);
        assert_eq!(directives[0].name, "crate1::mod1".to_string());
        assert_eq!(directives[0].level, LevelFilter::Info);

        assert_eq!(directives[1].name, "crate1::mod2".to_string());
        assert_eq!(directives[1].level, LevelFilter::Debug);

        assert_eq!(directives[2].name, "crate2".to_string());
        assert_eq!(directives[2].level, LevelFilter::Trace);
    }

    #[test]
    fn parse_env_invalid_crate() {
        let directives = parse_env("crate1::mod=warn=info,crate2=warn");
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].name, "crate2".to_string());
        assert_eq!(directives[0].level, LevelFilter::Warn);
    }

    #[test]
    fn parse_env_invalid_level() {
        let directives = parse_env("crate1::mod=wrong,crate2=error");
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].name, "crate2".to_string());
        assert_eq!(directives[0].level, LevelFilter::Error);
    }

    #[test]
    fn parse_env_empty() {
        let directives = parse_env("crate1::mod=,=trace");
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].name, "crate1::mod".to_string());
        assert_eq!(directives[0].level, LevelFilter::Info);
    }
}
