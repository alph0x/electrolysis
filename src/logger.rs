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
            eprintln!("{msg}");
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TestLogger {
        level: LogLevel,
        messages: std::cell::RefCell<Vec<(LogLevel, String)>>,
    }

    impl TestLogger {
        fn new(level: LogLevel) -> Self {
            Self {
                level,
                messages: std::cell::RefCell::new(vec![]),
            }
        }
        fn push(&self, level: LogLevel, msg: &str) {
            if self.level >= level {
                self.messages.borrow_mut().push((level, msg.to_string()));
            }
        }
        fn messages(&self) -> Vec<(LogLevel, String)> {
            self.messages.borrow().clone()
        }
    }

    impl Logger for TestLogger {
        fn info(&self, msg: &str) {
            self.push(LogLevel::Normal, msg);
        }
        fn verbose(&self, msg: &str) {
            self.push(LogLevel::Verbose, msg);
        }
        fn warn(&self, msg: &str) {
            self.push(LogLevel::Normal, msg);
        }
        fn error(&self, msg: &str) {
            self.push(LogLevel::Quiet, msg);
        }
    }

    #[test]
    fn log_level_ordering() {
        assert!(LogLevel::Quiet < LogLevel::Normal);
        assert!(LogLevel::Normal < LogLevel::Verbose);
    }

    #[test]
    fn quiet_level_blocks_info_and_warn() {
        let logger = TestLogger::new(LogLevel::Quiet);
        logger.info("hello");
        logger.warn("careful");
        assert!(logger.messages().is_empty());
    }

    #[test]
    fn quiet_level_allows_errors() {
        let logger = TestLogger::new(LogLevel::Quiet);
        logger.error("boom");
        assert_eq!(logger.messages().len(), 1);
        assert_eq!(logger.messages()[0].1, "boom");
    }

    #[test]
    fn normal_level_blocks_verbose() {
        let logger = TestLogger::new(LogLevel::Normal);
        logger.verbose("debug");
        assert!(logger.messages().is_empty());
    }

    #[test]
    fn normal_level_allows_info_and_warn() {
        let logger = TestLogger::new(LogLevel::Normal);
        logger.info("hello");
        logger.warn("careful");
        assert_eq!(logger.messages().len(), 2);
    }

    #[test]
    fn verbose_level_allows_everything() {
        let logger = TestLogger::new(LogLevel::Verbose);
        logger.info("hello");
        logger.verbose("debug");
        logger.warn("careful");
        logger.error("boom");
        assert_eq!(logger.messages().len(), 4);
    }

    #[test]
    fn null_logger_never_logs() {
        let logger = NullLogger;
        logger.info("hello");
        logger.verbose("debug");
        logger.warn("careful");
        logger.error("boom");
        // NullLogger no tiene estado, solo verificamos que no panickee.
    }

    #[test]
    fn default_log_level_is_normal() {
        assert_eq!(LogLevel::default(), LogLevel::Normal);
    }
}
