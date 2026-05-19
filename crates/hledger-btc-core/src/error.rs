#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),
    #[error("secret resolution failed: {0}")]
    Secret(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
