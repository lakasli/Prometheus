pub mod api;
pub mod auth;
pub mod character;
pub mod character_api;
pub mod character_import;
pub mod config;
pub mod conversation_summary;
pub mod error;
pub mod image_analysis;
pub mod image_analysis_api;
pub mod memory;
pub mod memory_api;
pub mod memory_candidate_api;
pub mod memory_candidates;
pub mod memory_commands;
pub mod memory_context;
pub mod qq;
pub mod state;
pub mod summary_api;
pub mod weixin_api;
pub mod weixin_login;
pub mod weixin_protocol;
pub mod weixin_runtime;
pub mod weixin_store;
pub mod worker;

pub fn build_sha() -> &'static str {
    option_env!("DSH_BUILD_SHA").unwrap_or("development")
}

#[cfg(test)]
mod tests;
