//! The suite of `memory.rs`, every test through the SQL fallback
//! (`MEMOIRE_MEMORY_SQL=1`): the engine run by the CLI over the query
//! route, which must print and fail as the memory API does.
#[path = "memory.rs"]
mod memory;
