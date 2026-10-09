//! Shared config, database access, wire types, and finding markdown for the
//! washburn services. Also owns the additive crawler-crate changes (see report).

pub mod blob;
pub mod config;
pub mod db;
pub mod markdown;
pub mod repo;
pub mod urls;
pub mod wire;

pub use blob::{compress, decompress, sha256};
pub use config::{CONFIG_ENV, DEFAULT_CONFIG_PATH, WashburnConfig, config_path_from_env_or_arg};
pub use db::{connect, migrate, worker_id};
pub use markdown::{ReportInput, level_one_header, level_one_section, serious_report};
pub use repo::*;
pub use urls::canonical_url;
pub use wire::{JudgeError, JudgeErrorKind, JudgeRequest, JudgeResponse};
