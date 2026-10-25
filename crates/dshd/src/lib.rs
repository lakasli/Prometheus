pub mod api;
pub mod auth;
pub mod character;
pub mod character_api;
pub mod character_import;
pub mod chat_reply;
pub mod config;
pub mod conversation_images;
pub mod conversation_images_api;
pub mod conversation_summary;
pub mod error;
pub mod image_analysis;
pub mod image_analysis_api;
pub mod media_assets;
pub mod memory;
pub mod memory_api;
pub mod memory_candidate_api;
pub mod memory_candidates;
pub mod memory_commands;
pub mod memory_context;
pub mod qq;
pub mod qq_api;
pub mod qq_media;
pub mod reply_delivery;
pub mod skill_drawing;
pub mod skills;
pub mod skills_api;
pub mod speech;
pub mod speech_api;
pub mod state;
pub mod summary_api;
pub mod trigger_api;
pub mod trigger_calendar;
pub mod trigger_execution;
pub mod trigger_retention;
pub mod trigger_rule;
pub mod trigger_scheduler;
pub mod trigger_store;
pub mod wasm_supervisor;
pub mod wasm_workbench;
pub mod wasm_workbench_api;
pub mod wasm_worker;
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
#[cfg(test)]
mod trigger_execution_tests;
