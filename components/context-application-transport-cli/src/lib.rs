mod error;
mod login;
mod role;
mod selection;
mod service;

pub use error::{Error, Result};

/// The running binary's name (`rmra`, `remora-etcher`), for the commands
/// messages suggest: both mount these commands, on the same contexts.
pub(crate) fn program() -> String {
    std::env::args_os()
        .next()
        .and_then(|arg0| {
            std::path::Path::new(&arg0)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "rmra".to_owned())
}
pub use login::{run_login, run_logout, run_whoami, LoginArgs, LogoutArgs, RoleArgs, WhoamiArgs};
pub use selection::{
    complete_contexts, complete_roles, context_on_command_line, ContextArgs, CONTEXT_ENV,
};
pub use service::{run, Command, DEFAULT_ADDRESS};
