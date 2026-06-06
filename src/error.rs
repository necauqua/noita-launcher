use thiserror::Error;

#[derive(Error, Debug)]
#[error("{message}")]
pub struct UserError {
    pub message: String,
    pub hint: Option<String>,
}

#[macro_export]
macro_rules! user_bail {
    ($message:expr, hint=$hint:expr $(,$arg:tt)* $(,)?) => {
        return Err(::eyre::eyre!($crate::error::UserError {
            message: format!($message, $($arg)*),
            hint: Some($hint.to_string()),
        }));
    };
    ($message:expr $(,$arg:tt)* $(,)?) => {
        return Err(::eyre::eyre!($crate::error::UserError {
            message: format!($message, $($arg)*),
            hint: None,
        }));
    };
}
