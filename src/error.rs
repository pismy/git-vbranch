#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git command failed: {0}")]
    Git(String),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("failed to parse API response: {0}")]
    Api(String),

    #[error("CI environment detection failed: {0}")]
    CiDetection(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
