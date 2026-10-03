mod error;
mod login;
mod role;
mod service;

pub use error::{Error, Result};
pub use login::{run_login, run_logout, run_whoami, LoginArgs, LogoutArgs, WhoamiArgs};
pub use role::{run as run_role, Command as RoleCommand};
pub use service::{run, Command, DEFAULT_ADDRESS};
