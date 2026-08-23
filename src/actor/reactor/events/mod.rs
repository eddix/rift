pub mod app;
pub mod command;
pub mod drag;
pub mod focus;
pub mod native_tab;
pub mod space;
pub mod system;
pub mod window;
pub mod window_discovery;

mod outcome;

pub(crate) use outcome::{EventOutcome, WindowDiscoveryRequest};
