mod error;
mod login;
mod role;
mod selection;
mod service;

pub use error::{hint, with_hint, Error, Result};
pub use login::{run_login, run_logout, run_whoami, LoginArgs, LogoutArgs, RoleArgs, WhoamiArgs};
pub use selection::{
    complete_contexts, complete_roles, context_on_command_line, ContextArgs, CONTEXT_ENV,
};
pub use service::{run, Command, DEFAULT_ADDRESS};
