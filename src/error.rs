use thiserror::Error;

#[derive(Error, Debug)]
pub enum ElectrolysisError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error at line {line}: {message}")]
    Parse { line: usize, message: String },

    #[error("Project structure error: {0}")]
    Structure(String),

    #[error("Invalid path: {0}")]
    #[allow(dead_code)]
    Path(String),
}

impl ElectrolysisError {
    pub fn parse(line: usize, message: impl Into<String>) -> Self {
        Self::Parse { line, message: message.into() }
    }

    pub fn structure(message: impl Into<String>) -> Self {
        Self::Structure(message.into())
    }
}
