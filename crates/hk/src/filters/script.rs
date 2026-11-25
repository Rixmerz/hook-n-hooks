//! `script`: a wrapper (`npm run`, `make`, `just`, `task`) whose script we
//! could not resolve. The command line says nothing, so the output decides:
//! the first filter whose [`Filter::sniff`] recognises the output handles it.
//! Unrecognised output falls back to the generic filter.

use super::{registry, Cmd, Filter};
use crate::config::Limits;

pub const NAME: &str = "script";

pub struct Script;

impl Filter for Script {
    fn name(&self) -> &'static str {
        NAME
    }

    /// Chosen explicitly by the hook (see `scripts.rs`), never by detection.
    fn detect(&self, _cmd: &Cmd) -> bool {
        false
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        registry()
            .iter()
            .find(|f| f.name() != NAME && f.sniff(output))?
            .filter(output, exit, limits)
    }
}
