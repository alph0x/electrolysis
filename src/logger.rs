use colored::Colorize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[derive(Default)]
pub enum LogLevel {
    Quiet = 0,
    #[default]
    Normal = 1,
    Verbose = 2,
}


pub trait Logger {
    fn info(&self, msg: &str);
    fn verbose(&self, msg: &str);
    fn warn(&self, msg: &str);
    fn error(&self, msg: &str);
}

pub struct ConsoleLogger {
    level: LogLevel,
}

impl ConsoleLogger {
    pub fn new(level: LogLevel) -> Self {
        Self { level }
    }
}

impl Logger for ConsoleLogger {
    fn info(&self, msg: &str) {
        if self.level >= LogLevel::Normal {
            eprintln!("{}", msg);
        }
    }

    fn verbose(&self, msg: &str) {
        if self.level >= LogLevel::Verbose {
            eprintln!("{}", msg.dimmed());
        }
    }

    fn warn(&self, msg: &str) {
        if self.level >= LogLevel::Normal {
            eprintln!("{} {}", "warn:".yellow(), msg);
        }
    }

    fn error(&self, msg: &str) {
        // Errors are always shown unless level is somehow below quiet.
        eprintln!("{} {}", "error:".red().bold(), msg);
    }
}

#[allow(dead_code)]
pub struct NullLogger;

impl Logger for NullLogger {
    fn info(&self, _msg: &str) {}
    fn verbose(&self, _msg: &str) {}
    fn warn(&self, _msg: &str) {}
    fn error(&self, _msg: &str) {}
}
