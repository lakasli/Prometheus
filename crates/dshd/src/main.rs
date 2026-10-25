use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dshd::{
    api, qq,
    state::{AppState, Options},
    weixin_login, weixin_runtime, worker,
};
use fs2::FileExt;
use serde_json::{json, Value};
use std::{net::SocketAddr, path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    name = "dshd",
    version,
    about = "DSH Rust AI assistant for Raspberry Pi"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(hide = true)]
    WasmWorker {
        #[arg(long, hide = true)]
        require_cgroup: bool,
    },
    Serve {
        #[arg(long, env = "DSH_DATA_DIR", default_value = "./data")]
        data_dir: PathBuf,
        #[arg(long, env = "DSH_LISTEN", default_value = "127.0.0.1:8419")]
        listen: SocketAddr,
        #[arg(long, env = "DSH_ORIGIN", default_value = "http://127.0.0.1:18419")]
        origin: String,
        #[arg(long, env = "DSH_WEB_DIR", default_value = "web/admin/dist")]
        web_dir: PathBuf,
        #[arg(long, env = "DSH_ALLOW_PRIVATE_MODEL", default_value_t = false)]
        allow_private_model: bool,
        #[arg(long, env = "DSH_SECURE_COOKIE", default_value_t = false)]
        secure_cookie: bool,
    },
    BootstrapToken {
        #[arg(long, env = "DSH_DATA_DIR", default_value = "./data")]
        data_dir: PathBuf,
    },
    Chat {
        #[arg(long, default_value = "http://127.0.0.1:18419")]
        url: String,
        #[arg(long, default_value = "admin")]
        username: String,
        #[arg(long)]
        conversation: Option<String>,
        #[arg(long, requires = "character_revision", conflicts_with = "conversation")]
        character_card: Option<String>,
        #[arg(long, requires = "character_card", conflicts_with = "conversation")]
        character_revision: Option<i64>,
        #[arg(long)]
        message: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Command::WasmWorker { require_cgroup } = &cli.command {
        std::process::exit(dshd::wasm_worker::run_stdio(*require_cgroup));
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_app(cli))
}

async fn run_app(cli: Cli) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "dshd=info".into()),
        )
        .init();
    match cli.command {
        Command::WasmWorker { .. } => unreachable!("worker runs before Tokio initialization"),
        Command::BootstrapToken { data_dir } => {
            let path = dshd::state::create_bootstrap_token(&data_dir)?;
            println!(
                "初始化凭证文件：{}（15分钟有效；请通过受保护终端读取，不要粘贴到日志）",
                path.display()
            );
        }
        Command::Serve {
            data_dir,
            listen,
            origin,
            web_dir,
            allow_private_model,
            secure_cookie,
        } => {
            let origin_url = url::Url::parse(origin.trim_end_matches('/'))?;
            anyhow::ensure!(
                origin_url.username().is_empty()
                    && origin_url.password().is_none()
                    && origin_url.query().is_none()
                    && origin_url.fragment().is_none()
                    && origin_url.path() == "/",
                "origin must be an exact HTTP origin without path"
            );
            let local = origin_url
                .host_str()
                .map(|v| {
                    v == "localhost"
                        || v.parse::<std::net::IpAddr>()
                            .map(|ip| ip.is_loopback())
                            .unwrap_or(false)
                })
                .unwrap_or(false);
            anyhow::ensure!(
                listen.ip().is_loopback(),
                "backend must listen on loopback; use SSH or a trusted HTTPS reverse proxy"
            );
            anyhow::ensure!(origin_url.scheme()=="https"&&secure_cookie||origin_url.scheme()=="http"&&local&&!secure_cookie,"HTTPS origin requires Secure cookies; HTTP is limited to explicit loopback tunnel origins");
            dshd::state::private_dir(&data_dir)?;
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(data_dir.join("daemon.lock"))?;
            lock.try_lock_exclusive()
                .context("another dshd process already uses this data directory")?;
            let state = AppState::open(Options {
                data_dir,
                origin: origin_url.origin().ascii_serialization(),
                allow_private_model,
                cookie_secure: secure_cookie,
                web_dir,
            })?;
            dshd::wasm_workbench::initialize_runtime(&state).await;
            state
                .db(|db| {
                    let tx = db.transaction()?;
                    dshd::trigger_store::service_started_in(&tx)?;
                    tx.commit()?;
                    Ok(())
                })
                .map_err(|_| anyhow::anyhow!("task event initialization unavailable"))?;
            let mut kernel = dsh_kernel::Kernel::new();
            let scheduler_state = state.clone();
            kernel
                .mount_task("task-trigger-scheduler", &[], move |cancel| {
                    dshd::trigger_scheduler::run(scheduler_state.clone(), cancel)
                })
                .await?;
            for index in 0..2 {
                let worker_state = state.clone();
                kernel
                    .mount_task(&format!("native-worker-{index}"), &[], move |cancel| {
                        worker::run_worker(worker_state.clone(), cancel)
                    })
                    .await?;
            }
            let outbox_state = state.clone();
            kernel
                .mount_task("qq-outbox", &[], move |cancel| {
                    dshd::reply_delivery::run(outbox_state.clone(), cancel)
                })
                .await?;
            let gateway_state = state.clone();
            let qq_receive_state = state.clone();
            kernel
                .mount_task("qq-receive", &[], move |cancel| {
                    qq::incoming_media::run(qq_receive_state.clone(), cancel)
                })
                .await?;
            kernel
                .mount_task("qq-gateway", &[], move |cancel| {
                    qq::run_gateway(gateway_state.clone(), cancel)
                })
                .await?;
            let cleanup_state = state.clone();
            let weixin_login_state = state.clone();
            kernel
                .mount_task("weixin-login", &[], move |cancel| {
                    weixin_login::run_login(weixin_login_state.clone(), cancel)
                })
                .await?;
            let weixin_receive_state = state.clone();
            kernel
                .mount_task("weixin-receiver", &[], move |cancel| {
                    weixin_runtime::run_receiver(weixin_receive_state.clone(), cancel)
                })
                .await?;
            let weixin_send_state = state.clone();
            kernel
                .mount_task("weixin-outbox", &[], move |cancel| {
                    weixin_runtime::run_sender(weixin_send_state.clone(), cancel)
                })
                .await?;
            kernel
                .mount_task("character-import-cleanup", &[], move |cancel| {
                    let state = cleanup_state.clone();
                    async move {
                        loop {
                            if dshd::character_api::cleanup_expired_imports(&state).is_err() {
                                tracing::warn!("character import cleanup unavailable");
                            }
                            if state
                                .db(|db| {
                                    dshd::speech::prune_in(db)?;
                                    dshd::media_assets::prune_in(db)?;
                                    Ok(())
                                })
                                .is_err()
                            {
                                tracing::warn!("expired media cleanup unavailable");
                            }
                            tokio::select! {
                                _ = cancel.cancelled() => break,
                                _ = tokio::time::sleep(Duration::from_secs(60)) => {},
                            }
                        }
                    }
                })
                .await?;
            let listener = tokio::net::TcpListener::bind(listen).await?;
            tracing::info!(address=%listen,version=env!("CARGO_PKG_VERSION"),commit=dshd::build_sha(),"DSH ready");
            let result = axum::serve(listener, api::router(state))
                .with_graceful_shutdown(shutdown())
                .await;
            kernel.shutdown().await?;
            result?;
        }
        Command::Chat {
            url,
            username,
            conversation,
            character_card,
            character_revision,
            message,
        } => {
            let character = character_card
                .zip(character_revision)
                .map(|(card_id, revision)| dshd::character::CardRef { card_id, revision });
            chat(&url, &username, conversation, character, &message).await?
        }
    }
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}};
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn chat(
    base: &str,
    username: &str,
    conversation: Option<String>,
    character: Option<dshd::character::CardRef>,
    message: &str,
) -> Result<()> {
    let endpoint = url::Url::parse(base)?;
    let local = endpoint
        .host_str()
        .map(|h| {
            h == "localhost"
                || h.parse::<std::net::IpAddr>()
                    .map(|ip| ip.is_loopback())
                    .unwrap_or(false)
        })
        .unwrap_or(false);
    anyhow::ensure!(
        endpoint.scheme() == "https" || endpoint.scheme() == "http" && local,
        "CLI requires HTTPS or loopback SSH tunnel"
    );
    let password = match std::env::var("DSH_ADMIN_PASSWORD") {
        Ok(value) => value,
        Err(_) => rpassword::prompt_password("管理密码：")?,
    };
    let origin = endpoint.origin().ascii_serialization();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(310))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let api = format!("{}/api/admin/v1", base.trim_end_matches('/'));
    let response = client
        .post(format!("{api}/auth/login"))
        .header("Origin", &origin)
        .json(&json!({"username":username,"password":password}))
        .send()
        .await?;
    anyhow::ensure!(
        response.status().is_success(),
        "CLI login failed (HTTP {})",
        response.status()
    );
    let cookie = response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.split(';').next())
        .ok_or_else(|| anyhow::anyhow!("missing session cookie"))?
        .to_string();
    let auth: Value = response.json().await?;
    let csrf = auth["csrf_token"].as_str().context("missing CSRF token")?;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("Origin", origin.parse()?);
    headers.insert("Cookie", cookie.parse()?);
    headers.insert("X-CSRF-Token", csrf.parse()?);
    let conversation = match conversation {
        Some(id) => id,
        None => {
            let response = client
                .post(format!("{api}/conversations"))
                .headers(headers.clone())
                .json(&json!({"title":"CLI对话","character":character}))
                .send()
                .await?;
            anyhow::ensure!(response.status().is_success(), "cannot create conversation");
            response.json::<Value>().await?["id"]
                .as_str()
                .context("missing conversation ID")?
                .to_string()
        }
    };
    let response = client
        .post(format!("{api}/conversations/{conversation}/messages"))
        .headers(headers.clone())
        .json(&json!({"content":message,"client_message_id":uuid::Uuid::new_v4().to_string()}))
        .send()
        .await?;
    anyhow::ensure!(response.status().is_success(), "cannot enqueue message");
    let run = response.json::<Value>().await?["run_id"]
        .as_str()
        .context("missing run ID")?
        .to_string();
    for _ in 0..620 {
        let response = client
            .get(format!("{api}/runs/{run}"))
            .headers(headers.clone())
            .send()
            .await?;
        let value: Value = response.json().await?;
        match value["status"].as_str().unwrap_or("") {
            "completed" => {
                println!("{}", value["response"].as_str().unwrap_or(""));
                return Ok(());
            }
            "failed" | "interrupted" | "cancelled" => {
                anyhow::bail!("{}", value["error"].as_str().unwrap_or("任务未完成"))
            }
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    anyhow::bail!("CLI polling timed out; task may still be running")
}
