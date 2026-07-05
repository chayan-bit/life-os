//! lifeos-memory - the cognitive memory engine (issues #111-#120,
//! docs/AI-MEMORY.md).
//!
//! Everything in this crate is a DERIVED, rebuildable projection of the
//! append-only `events` log. Events flow in from every tier - the Telegram
//! bot, the platform UI/API, the Mac harness, terminal hooks - because they
//! all write the same `events` table; the projector is source-agnostic, so
//! "collect memory from everywhere" is the substrate, not a feature flag.
//!
//! Module map (one concern per file, docs/AI-MEMORY.md section in brackets):
//! - `project`    event-sourced projection + deterministic rebuild [§2-3]
//! - `model`      MemoryModel trait + BLAKE3 replay cache + heuristic impl [§2]
//! - `retrieval`  activation-scored recall (RRF · recency · importance · freq) [§4]
//! - `gate`       self-RAG gate + multi-hop detector (deterministic) [§4]
//! - `graph`      petgraph spreading activation, 1-2 hops, tiered [§4]
//! - `communities` GraphRAG global queries: label propagation + cluster
//!   summaries + map-reduce-lite `ask_network`, rebuilt each sleep cycle [§13]
//! - `consolidate` sleep jobs: segment/consolidate/importance/surprise/
//!   decay/supersede - all writing events, never mutating [§5]
//! - `compiler`   deterministic token-budgeted context compiler [§6]
//! - `tier`       cold-tier migration to storage backends + promote [§7]
//! - `procedural` behavioral-rule store feeding the system prompt [§8]
//! - `redact`     the no-secret-in-memory guard, applied at projection time

pub mod communities;
pub mod compiler;
pub mod consolidate;
pub mod error;
pub mod gate;
pub mod graph;
pub mod model;
pub mod procedural;
pub mod project;
pub mod redact;
pub mod retrieval;
pub mod tier;

#[cfg(test)]
pub(crate) mod testutil;

pub use communities::{
    ask_network, detect_communities, list_network_map, rebuild_communities, Community,
    CommunitySummarizer, HeuristicSummarizer, MemberSnippet, NetworkAnswerHit, NetworkCommunity,
};
pub use compiler::{compile_context, BudgetSpec, CompiledContext, Section, Turn};
pub use consolidate::{run_sleep_cycle, unconsolidated_importance, SleepReport};
pub use error::MemoryError;
pub use gate::{is_multi_hop, needs_memory};
pub use graph::expand_seeds;
pub use model::{HeuristicModel, MemoryModel, ReplayCachedModel};
pub use procedural::{rules_for_prompt, HeuristicPolicyLearner, PolicyLearner, RuleDelta};
pub use project::{project_workspace, rebuild_workspace, ProjectionStats};
pub use retrieval::{
    recall, ActivationBreakdown, NoopVectorSearcher, RecallOutcome, RecallParams, RecalledMemory,
    VectorHit, VectorSearcher,
};
pub use tier::{find_cold_nodes, promote_nodes, tier_out_cold, TierReport};
