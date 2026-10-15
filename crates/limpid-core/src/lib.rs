pub mod agent_page;
pub mod claude;
pub mod codex;
pub mod grammars;
pub mod highlight;
pub mod pi;
pub mod render;
pub mod replies;
pub mod settings;
pub mod spec;
pub mod transport;

pub const AGENT_INSTRUCTIONS: &str = "Put questions that need the user's answer under a `## Questions` heading as a numbered list; give suggested answers as nested `- ` items.";
