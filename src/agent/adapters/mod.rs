pub mod agy;
pub mod claude;
pub mod codex;

use crate::agent::definition::AgentDefinition;
use crate::harness::Harness;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub trait HarnessAdapter {
    fn harness(&self) -> Harness;
    fn version(&self) -> u32;
    fn render(
        &self,
        definition: &AgentDefinition,
        enabled: bool,
        ctx: &crate::agent::artifacts::RenderContext,
    ) -> BTreeMap<PathBuf, Vec<u8>>;
}
