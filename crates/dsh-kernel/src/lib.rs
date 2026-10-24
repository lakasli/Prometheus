//! DSH's small application boundary around the real Cordis runtime.
//!
//! Task factories must cooperate with cancellation. Cordis owns their cleanup;
//! dropping a handle alone is deliberately not treated as shutdown.

use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use cordis::{Context, FiberState, InjectSpec, Plugin, PreparedPlugin, Service};
pub use tokio_util::sync::CancellationToken;

type TaskFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type TaskFactory = Arc<dyn Fn(CancellationToken) -> TaskFuture + Send + Sync>;

#[derive(Debug)]
pub struct KernelError(String);

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for KernelError {}

#[derive(Debug, Clone)]
pub struct PluginStatus {
    pub name: String,
    pub state: FiberState,
}

/// Explicit owner of every mounted application fiber.
pub struct Kernel {
    context: Context,
    fibers: Vec<(String, cordis::FiberHandle)>,
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel {
    pub fn new() -> Self {
        Self {
            context: Context::new(),
            fibers: Vec::new(),
        }
    }

    /// The typed Cordis service boundary. This is not a tenant authorization API.
    pub fn context(&self) -> &Context {
        &self.context
    }

    pub async fn mount_task<F, Fut>(
        &mut self,
        name: &str,
        dependencies: &[&str],
        task: F,
    ) -> Result<(), KernelError>
    where
        F: Fn(CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        if self.fibers.iter().any(|(existing, _)| existing == name) {
            return Err(KernelError(format!("plugin {name} is already mounted")));
        }
        let factory: TaskFactory = Arc::new(move |cancel| Box::pin(task(cancel)));
        let plugin = ManagedTask {
            dependencies: dependencies.iter().map(|name| (*name).to_owned()).collect(),
            factory,
        };
        let fiber = self
            .context
            .spawn(PreparedPlugin::from_input(plugin, ()))
            .await
            .map_err(|error| KernelError(error.to_string()))?;
        self.fibers.push((name.to_owned(), fiber));
        Ok(())
    }

    pub fn statuses(&self) -> Vec<PluginStatus> {
        self.fibers
            .iter()
            .map(|(name, fiber)| PluginStatus {
                name: name.clone(),
                state: fiber.state(),
            })
            .collect()
    }

    /// Stop in reverse mount order and attempt every cleanup even on failure.
    pub async fn shutdown(&mut self) -> Result<(), KernelError> {
        let mut failures = Vec::new();
        for (name, fiber) in self.fibers.drain(..).rev() {
            if let Err(error) = fiber.dispose().await {
                failures.push(format!("{name}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(KernelError(failures.join("; ")))
        }
    }
}

struct ManagedTask {
    dependencies: Vec<String>,
    factory: TaskFactory,
}

impl Plugin for ManagedTask {
    type Config = ();
    type Input = ();
    type PrepareError = Infallible;
    type ApplyError = KernelError;

    fn prepare(&self, _: ()) -> Result<(), Infallible> {
        Ok(())
    }

    fn inject(&self) -> InjectSpec {
        self.dependencies
            .iter()
            .fold(InjectSpec::none(), |spec, dependency| {
                spec.require(dependency.clone())
            })
    }

    async fn apply(&self, ctx: Context, _: &()) -> Result<(), KernelError> {
        let cancel = CancellationToken::new();
        let future = (self.factory)(cancel.clone());
        let owned = OwnedTask {
            cancel,
            join: Some(tokio::spawn(future)),
        };
        // One cleanup owns both cancellation and join, avoiding teardown order
        // deadlocks. If registration refuses, dropping OwnedTask aborts it.
        ctx.effect(move || async move { owned.stop().await })
            .map_err(|error| KernelError(error.to_string()))?;
        Ok(())
    }
}

struct OwnedTask {
    cancel: CancellationToken,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl OwnedTask {
    async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            // A non-cooperative task cannot block the rest of application exit.
            let abort = join.abort_handle();
            if tokio::time::timeout(std::time::Duration::from_secs(5), join)
                .await
                .is_err()
            {
                abort.abort();
            }
        }
    }
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(join) = &self.join {
            join.abort();
        }
    }
}
