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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_formatting() {
        let err = ElectrolysisError::parse(42, "unexpected token");
        assert_eq!(
            err.to_string(),
            "Parse error at line 42: unexpected token"
        );
    }

    #[test]
    fn structure_error_formatting() {
        let err = ElectrolysisError::structure("missing root object");
        assert_eq!(
            err.to_string(),
            "Project structure error: missing root object"
        );
    }

    #[test]
    fn io_error_from_std_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file gone");
        let err = ElectrolysisError::from(io_err);
        assert_eq!(
            err.to_string(),
            "I/O error: file gone"
        );
    }

    #[test]
    fn path_error_formatting() {
        let err = ElectrolysisError::Path("/bad/path".to_string());
        assert_eq!(
            err.to_string(),
            "Invalid path: /bad/path"
        );
    }

    #[test]
    fn parse_accepts_different_message_types() {
        // String
        let _ = ElectrolysisError::parse(1, "owned".to_string());
        // &str
        let _ = ElectrolysisError::parse(2, "borrowed");
        // Any Into<String>
        let _ = ElectrolysisError::parse(3, String::from("from"));
    }

    #[test]
    fn structure_accepts_different_message_types() {
        let _ = ElectrolysisError::structure("borrowed");
        let _ = ElectrolysisError::structure("owned".to_string());
        let _ = ElectrolysisError::structure(String::from("from"));
    }
}
