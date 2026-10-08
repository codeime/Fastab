//! Numeric resource snapshots. Counters are cumulative for one Engine lifetime.

/// One hook cache map, measured without expiring entries or exposing keys.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CacheMapDiagnostics {
    pub entries: usize,
    /// Estimated retained payload bytes, not allocator usage or process footprint.
    /// Omits map nodes, string spare capacity and allocator overhead. Shared
    /// option Arcs may be counted again across separate cached spec entries.
    pub allocated_bytes: usize,
    pub hits: u64,
    pub misses: u64,
    /// Also included in `misses`; expiration uses the reading caller's TTL.
    pub expired_removals: u64,
    /// Whole-map clears at capacity, excluding explicit clears and replacement.
    pub capacity_clears: u64,
}

/// Independently locked snapshots of the three per-engine hook maps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookDiagnostics {
    pub suggestions: CacheMapDiagnostics,
    pub script_output: CacheMapDiagnostics,
    pub specs: CacheMapDiagnostics,
}

/// Parsed file resources; counts deduplicate paths, bytes deduplicate live trees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RegistryDiagnostics {
    /// Unpinned file paths participating in idle release (aliases count once).
    pub cached_file_count: usize,
    pub idle_file_count: usize,
    /// Parsed tree payload estimate, excluding indexes and allocator overhead.
    pub allocated_bytes: usize,
    pub next_deadline_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HistoryDiagnostics {
    pub line_count: usize,
    pub index_count: usize,
    /// History lines and indexed values, excluding map nodes and allocator overhead.
    pub allocated_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EngineDiagnostics {
    pub registry: RegistryDiagnostics,
    pub hooks: HookDiagnostics,
    pub history: HistoryDiagnostics,
}

/// Worker lifetime counters. `cancelled` includes jobs skipped before execution;
/// `failed` includes initialization failures, watchdog timeouts and panics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestDiagnostics {
    pub submitted: u64,
    pub started: u64,
    pub completed: u64,
    pub cancelled: u64,
    pub failed: u64,
    pub engine_initializations: u64,
    pub watchdog_timeouts: u64,
    pub panics: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EngineClientDiagnostics {
    pub engine: Option<EngineDiagnostics>,
    pub requests: RequestDiagnostics,
}
