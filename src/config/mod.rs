//! Layered settings for both modes: flag > `VORP_*` environment > YAML file >
//! built-in default.
//!
//! Parsing and merging are pure functions. Only the `load` module reads files and the
//! process environment.

mod agent;
mod env;
mod error;
mod load;
mod parse;
mod relay;
mod setting;

pub(crate) use agent::{AgentLayer, AgentSettings};
pub(crate) use env::Env;
pub(crate) use error::ConfigError;
pub(crate) use load::{agent_config_path, read_layer, relay_config_path, vorp_config_dir};
pub(crate) use relay::{DevOptions, LimitsLayer, RelayLayer, RelaySettings, Signup, TlsLayer};
pub(crate) use setting::{Row, Setting, Source, write_rows};
