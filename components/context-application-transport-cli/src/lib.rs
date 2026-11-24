mod error;
mod login;
mod selection;
mod service;

pub use error::{hint, with_hint, Error, Result};
pub use login::{run_login, run_logout, run_whoami, LoginArgs, LogoutArgs, WhoamiArgs};
pub use selection::{
    complete_contexts, context_on_command_line, ContextArgs, ADDRESS_ENV, ASSUME_ROLE_ENV,
    CONTEXT_ENV, DEFINED_NAME, PLAINTEXT_ENV, SERVER_NAME_ENV, TOKEN_ENV,
};
pub use service::{migrate_on_start, run, Command, DEFAULT_ADDRESS};
