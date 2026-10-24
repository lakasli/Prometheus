pub mod api;
pub mod auth;
pub mod character;
pub mod character_api;
pub mod character_import;
pub mod config;
pub mod error;
pub mod memory;
pub mod memory_api;
pub mod memory_commands;
pub mod memory_context;
pub mod qq;
pub mod state;
pub mod worker;

pub fn build_sha() -> &'static str {
    option_env!("DSH_BUILD_SHA").unwrap_or("development")
}

#[cfg(test)]
mod tests;
