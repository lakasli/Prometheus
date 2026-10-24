pub mod api;
pub mod auth;
pub mod config;
pub mod error;
pub mod qq;
pub mod state;
pub mod worker;

pub fn build_sha() -> &'static str {
    option_env!("DSH_BUILD_SHA").unwrap_or("development")
}

#[cfg(test)]
mod tests;
