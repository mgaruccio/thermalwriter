use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("layout not found: {0}")]
    LayoutNotFound(String),

    #[error("invalid layout name: {0}")]
    InvalidLayout(String),

    #[error("invalid variable: {0}")]
    InvalidVariable(String),

    #[error("failed to read layout: {0}")]
    LayoutIo(String),

    #[error("failed to load config: {0}")]
    Config(String),

    #[error("failed to save config: {0}")]
    ConfigWrite(String),

    #[error("renderer error: {0}")]
    Render(String),

    #[error("renderer state poisoned")]
    StatePoisoned,

    #[error(
        "daemon is not running ({reason}). Start it with `systemctl --user start thermalwriter`."
    )]
    DaemonUnavailable { reason: String },

    #[error("daemon call failed: {0}")]
    DaemonCall(String),

    #[error("invalid background name: {0}")]
    InvalidBackground(String),

    #[error("background not found: {0}")]
    BackgroundNotFound(String),

    #[error("failed to read/write background: {0}")]
    BackgroundIo(String),

    #[error("no stream frame available: {0}")]
    NoFrame(String),
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_plain_string_not_object() {
        let err = AppError::LayoutNotFound("missing.svg".into());
        let json = serde_json::to_string(&err).expect("serialize");
        assert_eq!(json, "\"layout not found: missing.svg\"");
    }

    #[test]
    fn daemon_unavailable_message_is_descriptive() {
        let err = AppError::DaemonUnavailable {
            reason: "session bus unreachable".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("daemon is not running"), "{msg}");
        assert!(msg.contains("session bus unreachable"), "{msg}");
        assert!(msg.contains("systemctl"), "{msg}");
    }

    #[test]
    fn each_variant_includes_inner_payload() {
        let cases = [
            AppError::LayoutNotFound("a.svg".into()),
            AppError::InvalidLayout("../escape".into()),
            AppError::InvalidVariable("color".into()),
            AppError::LayoutIo("permission denied".into()),
            AppError::Config("bad toml".into()),
            AppError::ConfigWrite("disk full".into()),
            AppError::Render("usvg failed".into()),
            AppError::DaemonCall("set_layout rejected".into()),
        ];
        for err in cases {
            let s = err.to_string();
            assert!(!s.is_empty(), "empty Display for {err:?}");
        }
    }
}
