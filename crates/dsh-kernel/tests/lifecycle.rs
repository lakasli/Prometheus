use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use cordis::event::{ListenerOptions, mapper_sync};
use cordis::{Context, FiberState, InjectSpec, Plugin, PreparedChange, PreparedPlugin, Service};
use dsh_kernel::Kernel;

struct Store;
impl Service for Store {
    const NAME: &'static str = "dsh-test-store";
}

struct Dependent {
    applies: Arc<AtomicUsize>,
    cleanups: Arc<AtomicUsize>,
}

impl Plugin for Dependent {
    type Config = ();
    type Input = ();
    type PrepareError = Infallible;
    type ApplyError = std::io::Error;

    fn prepare(&self, _: ()) -> Result<(), Infallible> {
        Ok(())
    }

    fn inject(&self) -> InjectSpec {
        InjectSpec::none().require(Store::NAME)
    }

    async fn apply(&self, ctx: Context, _: &()) -> Result<(), Self::ApplyError> {
        ctx.try_service::<Store>()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        self.applies.fetch_add(1, Ordering::SeqCst);
        let cleaned = self.cleanups.clone();
        ctx.effect_sync(move || {
            cleaned.fetch_add(1, Ordering::SeqCst);
        })
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(())
    }
}

#[tokio::test]
async fn dependency_loss_cleans_generation_and_recovery_reapplies() {
    let root = Context::new();
    let applies = Arc::new(AtomicUsize::new(0));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let fiber = root
        .spawn(PreparedPlugin::from_input(
            Dependent {
                applies: applies.clone(),
                cleanups: cleanups.clone(),
            },
            (),
        ))
        .await
        .unwrap();
    assert_eq!(fiber.state(), FiberState::Pending);
    let first = root.provide(Arc::new(Store)).unwrap();
    assert_eq!(fiber.ready().await.unwrap(), FiberState::Active);
    assert_eq!(applies.load(Ordering::SeqCst), 1);
    first.remove().unwrap();
    assert_eq!(fiber.ready().await.unwrap(), FiberState::Pending);
    assert_eq!(cleanups.load(Ordering::SeqCst), 1);
    let second = root.provide(Arc::new(Store)).unwrap();
    assert_eq!(fiber.ready().await.unwrap(), FiberState::Active);
    assert_eq!(applies.load(Ordering::SeqCst), 2);
    fiber.dispose().await.unwrap();
    second.remove().unwrap();
    assert_eq!(cleanups.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn repeated_dispose_does_not_repeat_cleanup() {
    let root = Context::new();
    let _store = root.provide(Arc::new(Store)).unwrap();
    let cleanups = Arc::new(AtomicUsize::new(0));
    let fiber = root
        .spawn(PreparedPlugin::from_input(
            Dependent {
                applies: Arc::new(AtomicUsize::new(0)),
                cleanups: cleanups.clone(),
            },
            (),
        ))
        .await
        .unwrap();
    fiber.dispose().await.unwrap();
    let _ = fiber.dispose().await;
    assert_eq!(fiber.state(), FiberState::Disposed);
    assert_eq!(cleanups.load(Ordering::SeqCst), 1);
}

struct Configured {
    seen: Arc<AtomicUsize>,
}

impl Plugin for Configured {
    type Config = usize;
    type Input = usize;
    type PrepareError = std::io::Error;
    type ApplyError = Infallible;

    fn prepare(&self, value: usize) -> Result<usize, Self::PrepareError> {
        if value == 0 {
            Err(std::io::Error::other("zero is not a valid configuration"))
        } else {
            Ok(value)
        }
    }

    async fn apply(&self, _: Context, input: &usize) -> Result<(), Infallible> {
        self.seen.store(*input, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn rejected_configuration_never_replaces_active_plugin() {
    let root = Context::new();
    let _validation = root
        .on_update::<Configured, _>(
            mapper_sync::<Configured, _>(|_, value| {
                if value == 0 {
                    Err(std::io::Error::other("zero rejected by update policy"))
                } else {
                    Ok(value)
                }
            }),
            ListenerOptions::default(),
        )
        .unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let plugin = Configured { seen: seen.clone() };
    let initial = plugin.prepare(7).unwrap();
    let fiber = root
        .spawn(PreparedPlugin::from_input(plugin, initial))
        .await
        .unwrap();
    let validation = Configured { seen: seen.clone() }.prepare(0);
    assert!(validation.is_err());
    assert_eq!(fiber.state(), FiberState::Active);
    assert_eq!(seen.load(Ordering::SeqCst), 7);
    assert!(
        fiber
            .update(PreparedChange::from_input::<Configured>(0))
            .await
            .is_err()
    );
    assert_eq!(fiber.state(), FiberState::Active);
    assert_eq!(seen.load(Ordering::SeqCst), 7);
    // A mismatching prepared change is also rejected by Cordis itself before
    // commit, independently of DSH's schema validation.
    assert!(
        fiber
            .update(PreparedChange::from_input::<Dependent>(()))
            .await
            .is_err()
    );
    assert_eq!(fiber.state(), FiberState::Active);
    assert_eq!(seen.load(Ordering::SeqCst), 7);
    fiber
        .update(PreparedChange::from_input::<Configured>(9))
        .await
        .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 9);
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn disposing_kernel_cancels_and_joins_inflight_task() {
    let mut kernel = Kernel::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let stopped = Arc::new(AtomicUsize::new(0));
    let start = started.clone();
    let stop = stopped.clone();
    kernel
        .mount_task("inflight-model-request", &[], move |cancel| {
            let start = start.clone();
            let stop = stop.clone();
            async move {
                start.notify_one();
                cancel.cancelled().await;
                stop.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    kernel.shutdown().await.unwrap();
    assert_eq!(stopped.load(Ordering::SeqCst), 1);
    assert!(kernel.statuses().is_empty());
    kernel.shutdown().await.unwrap();
    assert_eq!(stopped.load(Ordering::SeqCst), 1);
}
