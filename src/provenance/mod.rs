pub mod authors;
pub mod blame;
pub mod record;

// Re-export commonly used items for convenience
pub use authors::{Author, AuthorScope, AuthorsCommand, OutputFormat};
pub use blame::{BlameContext, LineAttribution};
pub use record::{
    AgentInfo, AgentType, AttributionConfidence, DetectionMethod, ProvenanceRecord,
};
