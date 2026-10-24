//! A single bounded QR login lifecycle. Private transport state is deliberately
//! separated from the small, owner-authenticated management projection.
use crate::{
    error::{ApiError, ApiResult},
    state::{now, private_write, AppState},
    weixin_protocol::{
        ApiBase, LoginCredentials, ProtocolError, QrCode, QrStatus, SecretToken, VerificationCode,
        WeixinClient,
    },
    weixin_store::{self, AccountInput, AccountSnapshot, StoreError},
};
use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::{future::Future, time::Duration};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const ACTIVE: &str = "'creating','waiting','scanned','need_verification','verifying','redirecting'";
const LOGIN_SECONDS: i64 = 300;

#[derive(Clone, Serialize)]
pub struct LoginView {
    pub id: String,
    pub status: String,
    pub revision: i64,
    pub expires_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct CheckView {
    pub id: String,
    pub status: String,
    pub requested_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CheckRequest {
    pub id: String,
    pub account_id: String,
    pub generation: String,
    pub revision: i64,
    pub requested_at: i64,
}

pub enum CheckOutcome {
    Passed,
    Failed(&'static str),
}

// No Debug or Serialize: this includes the private QR identifier and a one-use
// verification code. Neither belongs in state diagnostics or configuration.
struct Job {
    id: String,
    owner: String,
    status: String,
    revision: i64,
    expected_account_revision: i64,
    expires_at: i64,
    qrcode: Option<String>,
    verification: Option<String>,
    redirect_base: Option<String>,
    redirects: i64,
}

pub fn revision_conflict() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "weixin_revision_conflict",
        "微信配置或扫码状态已变化，请刷新后再操作",
    )
}

pub fn store_error(error: StoreError) -> ApiError {
    match error {
        StoreError::Conflict | StoreError::StaleGeneration => revision_conflict(),
        StoreError::NotConfigured => ApiError::bad("weixin_not_configured", "请先完成微信扫码登录"),
        StoreError::Disabled => ApiError::bad("weixin_disabled", "请先启用微信账户"),
        StoreError::Invalid(_) => ApiError::bad(
            "weixin_invalid_configuration",
            "微信账户配置不可用，请检查登录状态",
        ),
        _ => ApiError::internal(),
    }
}

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("SAVEPOINT weixin_login_initialize")?;
    let result = (|| -> anyhow::Result<()> {
        db.execute_batch("CREATE TABLE IF NOT EXISTS weixin_login_schema(version INTEGER PRIMARY KEY,created_at INTEGER NOT NULL);")?;
        let version: i64 = db.query_row(
            "SELECT coalesce(max(version),0) FROM weixin_login_schema",
            [],
            |r| r.get(0),
        )?;
        anyhow::ensure!(version <= 1, "Unsupported weixin login schema");
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS weixin_login_jobs(
               id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,status TEXT NOT NULL,revision INTEGER NOT NULL,
               expected_account_revision INTEGER NOT NULL,expires_at INTEGER NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,
               qrcode TEXT,display_content TEXT,verification_code TEXT,redirect_base TEXT,redirects INTEGER NOT NULL DEFAULT 0,error_code TEXT);
             CREATE UNIQUE INDEX IF NOT EXISTS weixin_one_active_login ON weixin_login_jobs((1))
               WHERE status IN ('creating','waiting','scanned','need_verification','verifying','redirecting');
             CREATE TABLE IF NOT EXISTS weixin_checks(
               id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,account_id TEXT NOT NULL,generation TEXT NOT NULL,revision INTEGER NOT NULL,
               status TEXT NOT NULL,requested_at INTEGER NOT NULL,completed_at INTEGER,error_code TEXT);
             CREATE UNIQUE INDEX IF NOT EXISTS weixin_one_pending_check ON weixin_checks((1)) WHERE status='pending';",
        )?;
        db.execute(
            "INSERT OR IGNORE INTO weixin_login_schema(version,created_at) VALUES(1,?1)",
            [now()],
        )?;
        db.execute(&format!("UPDATE weixin_login_jobs SET status='interrupted',revision=revision+1,error_code='service_restarted',qrcode=NULL,display_content=NULL,verification_code=NULL,redirect_base=NULL,updated_at=?1 WHERE status IN ({ACTIVE})"),[now()])?;
        db.execute("UPDATE weixin_checks SET status='failed',completed_at=?1,error_code='service_restarted' WHERE status='pending'",[now()])?;
        maintenance(db)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            db.execute_batch("RELEASE weixin_login_initialize")?;
            Ok(())
        }
        Err(error) => {
            let _ = db.execute_batch(
                "ROLLBACK TO weixin_login_initialize; RELEASE weixin_login_initialize;",
            );
            Err(error)
        }
    }
}

fn maintenance(db: &Connection) -> anyhow::Result<()> {
    db.execute(&format!("UPDATE weixin_login_jobs SET status='expired',revision=revision+1,error_code='login_expired',qrcode=NULL,display_content=NULL,verification_code=NULL,redirect_base=NULL,updated_at=?1 WHERE expires_at<=?1 AND status IN ({ACTIVE})"),[now()])?;
    // Enforce cleanup even if a previous version stopped between terminal state
    // and maintenance; a terminal row must never retain private QR/code values.
    db.execute(&format!("UPDATE weixin_login_jobs SET qrcode=NULL,display_content=NULL,verification_code=NULL,redirect_base=NULL WHERE status NOT IN ({ACTIVE}) AND (qrcode IS NOT NULL OR display_content IS NOT NULL OR verification_code IS NOT NULL OR redirect_base IS NOT NULL)"),[])?;
    db.execute(&format!("DELETE FROM weixin_login_jobs WHERE id IN(SELECT id FROM weixin_login_jobs WHERE status NOT IN ({ACTIVE}) ORDER BY created_at DESC,rowid DESC LIMIT -1 OFFSET 32)"),[])?;
    db.execute("DELETE FROM weixin_checks WHERE id IN(SELECT id FROM weixin_checks WHERE status!='pending' ORDER BY requested_at DESC,rowid DESC LIMIT -1 OFFSET 32)",[])?;
    Ok(())
}

fn account_revision(db: &Connection) -> ApiResult<i64> {
    Ok(weixin_store::snapshot(db)
        .map_err(store_error)?
        .map_or(0, |account| account.revision))
}

pub fn login_view(db: &Connection, owner: &str, id: &str) -> ApiResult<LoginView> {
    maintenance(db).map_err(|_| ApiError::internal())?;
    db.query_row("SELECT id,status,revision,expires_at,display_content,error_code FROM weixin_login_jobs WHERE id=?1 AND owner_id=?2",params![id,owner],|r|Ok(LoginView {id:r.get(0)?,status:r.get(1)?,revision:r.get(2)?,expires_at:r.get(3)?,qr_content:r.get(4)?,error_code:r.get(5)?})).optional().map_err(|_|ApiError::internal())?.ok_or_else(ApiError::not_found)
}

pub fn pending_login(db: &Connection, owner: &str) -> ApiResult<Option<LoginView>> {
    maintenance(db).map_err(|_| ApiError::internal())?;
    let id:Option<String>=db.query_row(&format!("SELECT id FROM weixin_login_jobs WHERE owner_id=?1 AND status IN ({ACTIVE}) ORDER BY rowid DESC LIMIT 1"),[owner],|r|r.get(0)).optional().map_err(|_|ApiError::internal())?;
    id.map(|id| login_view(db, owner, &id)).transpose()
}

pub fn start_login(db: &mut Connection, owner: &str, expected: i64) -> ApiResult<LoginView> {
    let tx = db.transaction().map_err(|_| ApiError::internal())?;
    maintenance(&tx).map_err(|_| ApiError::internal())?;
    if account_revision(&tx)? != expected {
        return Err(revision_conflict());
    }
    let active: bool = tx
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM weixin_login_jobs WHERE status IN ({ACTIVE}))"),
            [],
            |r| r.get(0),
        )
        .map_err(|_| ApiError::internal())?;
    if active {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "weixin_revision_conflict",
            "已有扫码任务，请完成或取消后再开始",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute("INSERT INTO weixin_login_jobs(id,owner_id,status,revision,expected_account_revision,expires_at,created_at,updated_at) VALUES(?1,?2,'creating',1,?3,?4,?5,?5)",params![id,owner,expected,now()+LOGIN_SECONDS,now()]).map_err(|_|ApiError::internal())?;
    tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'weixin.login.start',?2,?3)",params![owner,id,now()]).map_err(|_|ApiError::internal())?;
    let result = login_view(&tx, owner, &id)?;
    tx.commit().map_err(|_| ApiError::internal())?;
    Ok(result)
}

pub fn submit_verification(
    db: &mut Connection,
    owner: &str,
    id: &str,
    expected: i64,
    code: &str,
) -> ApiResult<LoginView> {
    VerificationCode::new(code)
        .map_err(|_| ApiError::bad("weixin_invalid_verification", "验证码须为1到16位数字"))?;
    let tx = db.transaction().map_err(|_| ApiError::internal())?;
    let current = login_view(&tx, owner, id)?;
    if current.revision != expected || current.status != "need_verification" {
        return Err(revision_conflict());
    }
    tx.execute("UPDATE weixin_login_jobs SET status='verifying',verification_code=?1,revision=revision+1,error_code=NULL,updated_at=?2 WHERE id=?3 AND revision=?4",params![code,now(),id,expected]).map_err(|_|ApiError::internal())?;
    tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'weixin.login.verify',?2,?3)",params![owner,id,now()]).map_err(|_|ApiError::internal())?;
    let result = login_view(&tx, owner, id)?;
    tx.commit().map_err(|_| ApiError::internal())?;
    Ok(result)
}

pub fn cancel_login(
    db: &mut Connection,
    owner: &str,
    id: &str,
    expected: i64,
) -> ApiResult<LoginView> {
    let tx = db.transaction().map_err(|_| ApiError::internal())?;
    let current = login_view(&tx, owner, id)?;
    if current.revision != expected {
        return Err(revision_conflict());
    }
    if !matches!(
        current.status.as_str(),
        "creating" | "waiting" | "scanned" | "need_verification" | "verifying" | "redirecting"
    ) {
        return Err(revision_conflict());
    }
    tx.execute("UPDATE weixin_login_jobs SET status='cancelled',revision=revision+1,qrcode=NULL,display_content=NULL,verification_code=NULL,redirect_base=NULL,error_code=NULL,updated_at=?1 WHERE id=?2 AND revision=?3",params![now(),id,expected]).map_err(|_|ApiError::internal())?;
    tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'weixin.login.cancel',?2,?3)",params![owner,id,now()]).map_err(|_|ApiError::internal())?;
    let result = login_view(&tx, owner, id)?;
    tx.commit().map_err(|_| ApiError::internal())?;
    Ok(result)
}

pub fn pending_check(db: &Connection) -> anyhow::Result<Option<CheckRequest>> {
    db.execute("UPDATE weixin_checks SET status='failed',completed_at=?1,error_code='check_expired' WHERE status='pending' AND requested_at<=?2",params![now(),now()-45])?;
    db.execute("UPDATE weixin_checks SET status='failed',completed_at=?1,error_code='account_changed' WHERE status='pending' AND NOT EXISTS(SELECT 1 FROM weixin_account a WHERE a.account_id=weixin_checks.account_id AND a.generation=weixin_checks.generation AND a.revision=weixin_checks.revision AND a.enabled=1)",[now()])?;
    Ok(db.query_row("SELECT id,account_id,generation,revision,requested_at FROM weixin_checks WHERE status='pending' ORDER BY rowid LIMIT 1",[],|r|Ok(CheckRequest {id:r.get(0)?,account_id:r.get(1)?,generation:r.get(2)?,revision:r.get(3)?,requested_at:r.get(4)?})).optional()?)
}

pub fn complete_check(
    db: &Connection,
    account: &AccountSnapshot,
    outcome: CheckOutcome,
) -> anyhow::Result<bool> {
    let (status, code) = match outcome {
        CheckOutcome::Passed => ("passed", None),
        CheckOutcome::Failed(code) => {
            anyhow::ensure!(
                !code.is_empty()
                    && code.len() <= 64
                    && code
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')),
                "Invalid safe check error code"
            );
            ("failed", Some(code))
        }
    };
    Ok(db.execute("UPDATE weixin_checks SET status=?1,completed_at=?2,error_code=?3 WHERE status='pending' AND account_id=?4 AND generation=?5 AND revision=?6 AND requested_at>?7 AND EXISTS(SELECT 1 FROM weixin_account a WHERE a.account_id=?4 AND a.generation=?5 AND a.revision=?6 AND a.enabled=1)",params![status,now(),code,account.account_id,account.generation,account.revision,now()-45])?==1)
}

pub fn latest_check(db: &Connection, owner: &str) -> anyhow::Result<Option<CheckView>> {
    pending_check(db)?;
    Ok(db.query_row("SELECT id,status,requested_at,completed_at,error_code FROM weixin_checks WHERE owner_id=?1 ORDER BY rowid DESC LIMIT 1",[owner],|r|Ok(CheckView{id:r.get(0)?,status:r.get(1)?,requested_at:r.get(2)?,completed_at:r.get(3)?,error_code:r.get(4)?})).optional()?)
}

pub fn start_check(db: &mut Connection, owner: &str, expected: i64) -> ApiResult<CheckView> {
    let tx = db.transaction().map_err(|_| ApiError::internal())?;
    pending_check(&tx).map_err(|_| ApiError::internal())?;
    let account = weixin_store::snapshot(&tx).map_err(store_error)?;
    if account.as_ref().map_or(0, |a| a.revision) != expected {
        return Err(revision_conflict());
    }
    let account = account.ok_or_else(|| store_error(StoreError::NotConfigured))?;
    if !account.enabled {
        return Err(store_error(StoreError::Disabled));
    }
    let active: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM weixin_checks WHERE status='pending')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| ApiError::internal())?;
    if active {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "weixin_revision_conflict",
            "连接检查正在等待接收器结果，请稍后查看",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute("INSERT INTO weixin_checks(id,owner_id,account_id,generation,revision,status,requested_at) VALUES(?1,?2,?3,?4,?5,'pending',?6)",params![id,owner,account.account_id,account.generation,account.revision,now()]).map_err(|_|ApiError::internal())?;
    tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'weixin.check.request',?2,?3)",params![owner,id,now()]).map_err(|_|ApiError::internal())?;
    let result = latest_check(&tx, owner)
        .map_err(|_| ApiError::internal())?
        .ok_or_else(ApiError::internal)?;
    tx.commit().map_err(|_| ApiError::internal())?;
    Ok(result)
}

fn load_active(state: &AppState) -> anyhow::Result<Option<Job>> {
    let db = state
        .0
        .db
        .lock()
        .map_err(|_| anyhow::anyhow!("Login database unavailable"))?;
    maintenance(&db)?;
    let row=db.query_row(&format!("SELECT id,owner_id,status,revision,expected_account_revision,expires_at,qrcode,verification_code,redirect_base,redirects FROM weixin_login_jobs WHERE status IN ({ACTIVE}) LIMIT 1"),[],|r|Ok(Job{id:r.get(0)?,owner:r.get(1)?,status:r.get(2)?,revision:r.get(3)?,expected_account_revision:r.get(4)?,expires_at:r.get(5)?,qrcode:r.get(6)?,verification:r.get(7)?,redirect_base:r.get(8)?,redirects:r.get(9)?})).optional()?;
    Ok(row)
}

fn guard(db: &Connection, job: &Job) -> anyhow::Result<bool> {
    let exists:bool=db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM weixin_login_jobs WHERE id=?1 AND revision=?2 AND expires_at>?3 AND status IN ({ACTIVE}))"),params![job.id,job.revision,now()],|r|r.get(0))?;
    let revision = weixin_store::snapshot(db)?.map_or(0, |a| a.revision);
    Ok(exists && revision == job.expected_account_revision)
}

fn terminal(state: &AppState, job: &Job, status: &str, code: Option<&str>) -> anyhow::Result<bool> {
    let db = state
        .0
        .db
        .lock()
        .map_err(|_| anyhow::anyhow!("Login database unavailable"))?;
    let changed=db.execute(&format!("UPDATE weixin_login_jobs SET status=?1,error_code=?2,revision=revision+1,qrcode=NULL,display_content=NULL,verification_code=NULL,redirect_base=NULL,updated_at=?3 WHERE id=?4 AND revision=?5 AND status IN ({ACTIVE})"),params![status,code,now(),job.id,job.revision])?==1;
    maintenance(&db)?;
    Ok(changed)
}

fn phase(state: &AppState, job: &Job, status: &str) -> anyhow::Result<()> {
    let db = state
        .0
        .db
        .lock()
        .map_err(|_| anyhow::anyhow!("Login database unavailable"))?;
    if guard(&db, job)? && job.status != status {
        db.execute("UPDATE weixin_login_jobs SET status=?1,revision=revision+1,error_code=NULL,updated_at=?2 WHERE id=?3 AND revision=?4",params![status,now(),job.id,job.revision])?;
    }
    Ok(())
}

fn consume_verification(
    state: &AppState,
    job: &mut Job,
) -> anyhow::Result<Option<VerificationCode>> {
    let Some(raw) = job.verification.take() else {
        return Ok(None);
    };
    let code = VerificationCode::new(raw)?;
    let db = state
        .0
        .db
        .lock()
        .map_err(|_| anyhow::anyhow!("Login database unavailable"))?;
    if !guard(&db, job)? {
        return Ok(None);
    }
    let changed=db.execute("UPDATE weixin_login_jobs SET verification_code=NULL,revision=revision+1,updated_at=?1 WHERE id=?2 AND revision=?3",params![now(),job.id,job.revision])?;
    if changed == 1 {
        job.revision += 1;
        Ok(Some(code))
    } else {
        Ok(None)
    }
}

fn save_credentials(
    state: &AppState,
    job: &Job,
    credentials: LoginCredentials,
) -> anyhow::Result<bool> {
    {
        let db = state
            .0
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("Login database unavailable"))?;
        if !guard(&db, job)? {
            return Ok(false);
        }
    }
    let reference = uuid::Uuid::new_v4().to_string();
    private_write(
        &state.0.options.data_dir.join("secrets").join(&reference),
        credentials.token.expose().as_bytes(),
    )?;
    // A failed CAS may leave a private, unreferenced file. Never remove a file
    // after an uncertain commit or overwrite a pre-existing credential.
    let mut db = state
        .0
        .db
        .lock()
        .map_err(|_| anyhow::anyhow!("Login database unavailable"))?;
    let tx = db.transaction()?;
    if !guard(&tx, job)? {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'weixin_bot_token',?2)",
        params![reference, now()],
    )?;
    weixin_store::replace_account_in(
        &tx,
        job.expected_account_revision,
        &job.owner,
        &AccountInput {
            account_id: credentials.account_id,
            base_url: credentials.base.as_str().into(),
            credential_ref: reference,
            linked_user_id: credentials.owner_id,
            enabled: true,
        },
    )?;
    tx.execute("UPDATE weixin_login_jobs SET status='confirmed',revision=revision+1,qrcode=NULL,display_content=NULL,verification_code=NULL,redirect_base=NULL,error_code=NULL,updated_at=?1 WHERE id=?2 AND revision=?3",params![now(),job.id,job.revision])?;
    tx.commit()?;
    state
        .0
        .weixin_signal
        .send_modify(|generation| *generation = generation.wrapping_add(1));
    Ok(true)
}

fn apply_status(state: &AppState, job: &Job, status: QrStatus) -> anyhow::Result<()> {
    match status {
        QrStatus::Waiting => phase(state, job, "waiting")?,
        QrStatus::Scanned => phase(state, job, "scanned")?,
        QrStatus::NeedVerification => phase(state, job, "need_verification")?,
        QrStatus::VerificationBlocked => {
            terminal(
                state,
                job,
                "verification_blocked",
                Some("verification_blocked"),
            )?;
        }
        QrStatus::Expired => {
            terminal(state, job, "expired", Some("login_expired"))?;
        }
        QrStatus::Confirmed(credentials) => {
            save_credentials(state, job, credentials)?;
        }
        QrStatus::Redirect(base) => {
            if job.redirects >= 3 {
                terminal(state, job, "failed", Some("redirect_limit"))?;
            } else {
                let db = state
                    .0
                    .db
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Login database unavailable"))?;
                if guard(&db, job)? {
                    db.execute("UPDATE weixin_login_jobs SET status='redirecting',redirect_base=?1,redirects=redirects+1,revision=revision+1,updated_at=?2 WHERE id=?3 AND revision=?4",params![base.as_str(),now(),job.id,job.revision])?;
                }
            }
        }
        QrStatus::AlreadyBound { account_id } => {
            let account = state
                .db(|db| Ok(weixin_store::snapshot(db)?))
                .map_err(|_| anyhow::anyhow!("Login account unavailable"))?;
            let matching = account.as_ref().is_some_and(|account| {
                account_id.as_deref() == Some(account.account_id.as_str())
                    && account.revision == job.expected_account_revision
            }) && account
                .as_ref()
                .is_some_and(|account| local_token(state, account).is_some());
            let mut db = state
                .0
                .db
                .lock()
                .map_err(|_| anyhow::anyhow!("Login database unavailable"))?;
            let tx = db.transaction()?;
            if !guard(&tx, job)? {
                return Ok(());
            }
            let current = weixin_store::snapshot(&tx)?;
            let matching = matching
                && current
                    .as_ref()
                    .zip(account.as_ref())
                    .is_some_and(|(current, previous)| {
                        current.generation == previous.generation
                            && current.credential_ref == previous.credential_ref
                    });
            tx.execute("UPDATE weixin_login_jobs SET status=?1,error_code=?2,revision=revision+1,qrcode=NULL,display_content=NULL,verification_code=NULL,redirect_base=NULL,updated_at=?3 WHERE id=?4 AND revision=?5",params![if matching {"existing_credentials"} else {"failed"},if matching {None} else {Some("existing_credentials_unavailable")},now(),job.id,job.revision])?;
            tx.commit()?;
        }
    }
    Ok(())
}

enum Awaited<T> {
    Value(T),
    Changed,
    Stopped,
    Expired,
}

fn local_token(state: &AppState, account: &AccountSnapshot) -> Option<SecretToken> {
    if account.connection_status == "needs_login" {
        return None;
    }
    state
        .secret(&account.credential_ref, "weixin_bot_token")
        .ok()
        .and_then(|raw| SecretToken::new(raw).ok())
        .filter(|token| token.expose().bytes().all(|byte| byte.is_ascii_graphic()))
}

async fn responsive<T>(
    operation: impl Future<Output = T>,
    signal: &mut watch::Receiver<u64>,
    cancel: &CancellationToken,
    expires_at: i64,
) -> Awaited<T> {
    let remaining = Duration::from_secs((expires_at - now()).clamp(0, LOGIN_SECONDS) as u64);
    tokio::select! {
        biased;
        _=cancel.cancelled()=>Awaited::Stopped,
        _=signal.changed()=>Awaited::Changed,
        _=tokio::time::sleep(remaining)=>Awaited::Expired,
        value=operation=>Awaited::Value(value),
    }
}

pub async fn run_login(state: AppState, cancellation: CancellationToken) {
    run_login_with_client(state, cancellation, WeixinClient::production()).await;
}

async fn run_login_with_client(
    state: AppState,
    cancellation: CancellationToken,
    client: WeixinClient,
) {
    let mut signal = state.0.weixin_signal.subscribe();
    loop {
        signal.borrow_and_update();
        if cancellation.is_cancelled() {
            break;
        }
        let job = match load_active(&state) {
            Ok(Some(job)) => job,
            _ => {
                tokio::select! {_=cancellation.cancelled()=>break,_=signal.changed()=>{},_=tokio::time::sleep(Duration::from_secs(1))=>{}}
                continue;
            }
        };
        let mut job = job;
        let account_current = state
            .db(|db| Ok(weixin_store::snapshot(db)?.map_or(0, |a| a.revision)))
            .ok();
        if account_current != Some(job.expected_account_revision) {
            let _ = terminal(&state, &job, "failed", Some("account_changed"));
            continue;
        }
        if job.status == "need_verification" && job.verification.is_none() {
            match responsive(
                std::future::pending::<()>(),
                &mut signal,
                &cancellation,
                job.expires_at,
            )
            .await
            {
                Awaited::Stopped => {
                    let _ = terminal(&state, &job, "interrupted", Some("service_stopped"));
                    break;
                }
                _ => continue,
            }
        }
        if job.qrcode.is_none() {
            let local = state
                .db(|db| Ok(weixin_store::snapshot(db)?))
                .ok()
                .flatten()
                .and_then(|account| local_token(&state, &account));
            let tokens = local.as_ref().into_iter().collect::<Vec<_>>();
            match responsive(
                client.fetch_qr(&tokens, &cancellation),
                &mut signal,
                &cancellation,
                job.expires_at,
            )
            .await
            {
                Awaited::Value(Ok(ticket)) => {
                    if let Ok(db) = state.0.db.lock() {
                        if guard(&db, &job).unwrap_or(false) {
                            let _=db.execute("UPDATE weixin_login_jobs SET status='waiting',revision=revision+1,qrcode=?1,display_content=?2,updated_at=?3 WHERE id=?4 AND revision=?5",params![ticket.qrcode.expose(),ticket.display_content.expose(),now(),job.id,job.revision]);
                        }
                    }
                }
                Awaited::Value(Err(_)) => {
                    let _ = terminal(&state, &job, "failed", Some("qr_fetch_failed"));
                }
                Awaited::Stopped => {
                    let _ = terminal(&state, &job, "interrupted", Some("service_stopped"));
                    break;
                }
                _ => {}
            }
            continue;
        }
        let code = match consume_verification(&state, &mut job) {
            Ok(code) => code,
            Err(_) => {
                let _ = terminal(&state, &job, "failed", Some("verification_unavailable"));
                continue;
            }
        };
        let qrcode = match QrCode::new(job.qrcode.as_deref().unwrap_or("")) {
            Ok(qrcode) => qrcode,
            Err(_) => {
                let _ = terminal(&state, &job, "failed", Some("qr_unavailable"));
                continue;
            }
        };
        let base = match job
            .redirect_base
            .as_deref()
            .map(ApiBase::trusted_https)
            .transpose()
        {
            Ok(base) => base,
            Err(_) => {
                let _ = terminal(&state, &job, "failed", Some("untrusted_redirect"));
                continue;
            }
        };
        match responsive(
            client.poll_qr(&qrcode, code.as_ref(), base.as_ref(), &cancellation),
            &mut signal,
            &cancellation,
            job.expires_at,
        )
        .await
        {
            Awaited::Value(Ok(status)) => {
                if apply_status(&state, &job, status).is_err() {
                    let _ = terminal(&state, &job, "failed", Some("login_storage_failed"));
                }
            }
            Awaited::Value(Err(error)) => {
                let code = if matches!(
                    error,
                    ProtocolError::UntrustedEndpoint | ProtocolError::PrivateAddress
                ) {
                    "untrusted_redirect"
                } else {
                    "qr_poll_failed"
                };
                let _ = terminal(&state, &job, "failed", Some(code));
            }
            Awaited::Stopped => {
                let _ = terminal(&state, &job, "interrupted", Some("service_stopped"));
                break;
            }
            _ => {}
        }
        tokio::select! {_=cancellation.cancelled()=>{},_=signal.changed()=>{},_=tokio::time::sleep(Duration::from_millis(250))=>{}}
    }
    if let Ok(Some(job)) = load_active(&state) {
        let _ = terminal(&state, &job, "interrupted", Some("service_stopped"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::Uri,
        routing::{get, post},
        Json, Router,
    };
    use serde_json::{json, Value};
    use std::{
        collections::VecDeque,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };
    use tempfile::TempDir;
    use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

    struct Reply {
        value: Value,
        held: bool,
    }
    struct Mock {
        replies: Mutex<VecDeque<Reply>>,
        calls: Mutex<Vec<String>>,
        fetches: AtomicUsize,
        held: Notify,
        release: Notify,
    }
    async fn fetch(State(mock): State<Arc<Mock>>) -> Json<Value> {
        mock.fetches.fetch_add(1, Ordering::SeqCst);
        Json(
            json!({"qrcode":"private-qr-identifier","qrcode_img_content":"https://weixin.qq.com/login?ticket=private-display"}),
        )
    }
    async fn poll(State(mock): State<Arc<Mock>>, uri: Uri) -> Json<Value> {
        mock.calls.lock().unwrap().push(uri.to_string());
        let reply = mock.replies.lock().unwrap().pop_front().unwrap_or(Reply {
            value: json!({"status":"wait"}),
            held: false,
        });
        if reply.held {
            mock.held.notify_one();
            mock.release.notified().await;
        }
        Json(reply.value)
    }
    fn open() -> (TempDir, AppState) {
        let temp = TempDir::new().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: temp.path().to_owned(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: temp.path().join("web"),
        })
        .unwrap();
        (temp, state)
    }
    async fn server(replies: Vec<(Value, bool)>) -> (WeixinClient, Arc<Mock>, JoinHandle<()>) {
        let mock = Arc::new(Mock {
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|(value, held)| Reply { value, held })
                    .collect(),
            ),
            calls: Mutex::new(Vec::new()),
            fetches: AtomicUsize::new(0),
            held: Notify::new(),
            release: Notify::new(),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, _) = WeixinClient::for_test(&format!("http://{address}/"), true).unwrap();
        let router = Router::new()
            .route("/ilink/bot/get_bot_qrcode", post(fetch))
            .route("/ilink/bot/get_qrcode_status", get(poll))
            .with_state(mock.clone());
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (client, mock, handle)
    }
    fn start(state: &AppState, expected: i64) -> LoginView {
        state
            .db(|db| Ok(start_login(db, "admin", expected)))
            .unwrap()
            .unwrap()
    }
    fn view(state: &AppState, id: &str) -> LoginView {
        state
            .db(|db| Ok(login_view(db, "admin", id)))
            .unwrap()
            .unwrap()
    }
    fn notify(state: &AppState) {
        state
            .0
            .weixin_signal
            .send_modify(|value| *value = value.wrapping_add(1));
    }
    async fn wait(state: &AppState, id: &str, status: &str) -> LoginView {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = view(state, id);
                if current.status == status {
                    return current;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("bounded login state transition did not complete")
    }
    fn confirmed() -> Value {
        json!({"status":"confirmed","bot_token":"private-issued-bot-token","ilink_bot_id":"bot@im.bot","ilink_user_id":"owner@im.wechat","baseurl":crate::weixin_protocol::DEFAULT_BASE})
    }
    fn credentials() -> LoginCredentials {
        LoginCredentials {
            token: SecretToken::new("late-private-bot-token").unwrap(),
            account_id: "late@im.bot".into(),
            owner_id: "owner@im.wechat".into(),
            base: ApiBase::trusted_https(crate::weixin_protocol::DEFAULT_BASE).unwrap(),
        }
    }
    fn configured(state: &AppState, valid_file: bool) -> AccountSnapshot {
        let reference = uuid::Uuid::new_v4().to_string();
        if valid_file {
            private_write(
                &state.0.options.data_dir.join("secrets").join(&reference),
                b"existing-private-bot-token",
            )
            .unwrap();
        }
        state
            .db(|db| {
                db.execute(
                    "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'weixin_bot_token',?2)",
                    params![reference, now()],
                )?;
                Ok(weixin_store::replace_account(
                    db,
                    0,
                    "admin",
                    &AccountInput {
                        account_id: "existing@im.bot".into(),
                        linked_user_id: "owner@im.wechat".into(),
                        base_url: crate::weixin_protocol::DEFAULT_BASE.into(),
                        credential_ref: reference,
                        enabled: false,
                    },
                ))
            })
            .unwrap()
            .unwrap()
    }
    async fn stop(cancel: CancellationToken, worker: JoinHandle<()>, server: JoinHandle<()>) {
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn managed_qr_verification_uses_code_once_and_confirmed_only_saves_connecting_credentials(
    ) {
        let (_temp, state) = open();
        let (client, mock, http) = server(vec![
            (json!({"status":"wait"}), false),
            (json!({"status":"scaned"}), false),
            (json!({"status":"need_verifycode"}), false),
            (confirmed(), true),
        ])
        .await;
        let created = start(&state, 0);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(run_login_with_client(state.clone(), cancel.clone(), client));
        let pending = wait(&state, &created.id, "need_verification").await;
        assert!(pending
            .qr_content
            .as_deref()
            .unwrap()
            .contains("private-display"));
        assert!(!serde_json::to_string(&pending)
            .unwrap()
            .contains("private-qr-identifier"));
        let count = mock.calls.lock().unwrap().len();
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(
            mock.calls.lock().unwrap().len(),
            count,
            "need_verification waits for a user code without polling or re-submission"
        );
        assert!(state
            .db(|db| Ok(submit_verification(
                db,
                "admin",
                &created.id,
                pending.revision,
                "123a"
            )))
            .unwrap()
            .is_err());
        state
            .db(|db| {
                Ok(submit_verification(
                    db,
                    "admin",
                    &created.id,
                    pending.revision,
                    "123456",
                ))
            })
            .unwrap()
            .unwrap();
        notify(&state);
        tokio::time::timeout(Duration::from_secs(2), mock.held.notified())
            .await
            .unwrap();
        state
            .db(|db| {
                let code: Option<String> = db.query_row(
                    "SELECT verification_code FROM weixin_login_jobs WHERE id=?1",
                    [&created.id],
                    |r| r.get(0),
                )?;
                assert!(
                    code.is_none(),
                    "verification code is cleared before dispatch"
                );
                Ok(())
            })
            .unwrap();
        mock.release.notify_one();
        let done = wait(&state, &created.id, "confirmed").await;
        assert!(done.qr_content.is_none());
        let account = state
            .db(|db| Ok(weixin_store::snapshot(db)?))
            .unwrap()
            .unwrap();
        assert!(account.enabled);
        assert_eq!(account.connection_status, "connecting");
        assert_eq!(
            state
                .secret(&account.credential_ref, "weixin_bot_token")
                .unwrap(),
            "private-issued-bot-token"
        );
        assert_eq!(
            mock.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|uri| uri.contains("verify_code=123456"))
                .count(),
            1
        );
        assert_eq!(
            mock.fetches.load(Ordering::SeqCst),
            1,
            "one login never automatically refreshes its QR"
        );
        state.db(|db| {assert_eq!(db.query_row("SELECT count(*) FROM weixin_login_jobs WHERE qrcode IS NOT NULL OR display_content IS NOT NULL OR verification_code IS NOT NULL",[],|r|r.get::<_,i64>(0))?,0);Ok(())}).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(
                    state
                        .0
                        .options
                        .data_dir
                        .join("secrets")
                        .join(&account.credential_ref)
                )
                .unwrap()
                .permissions()
                .mode()
                    & 0o777,
                0o600
            );
        }
        stop(cancel, worker, http).await;
    }

    #[tokio::test]
    async fn cancellation_and_expiry_reject_a_late_real_protocol_confirmation() {
        for expired in [false, true] {
            let (_temp, state) = open();
            let (client, mock, http) = server(vec![(confirmed(), true)]).await;
            let created = start(&state, 0);
            let cancel = CancellationToken::new();
            let worker = tokio::spawn(run_login_with_client(state.clone(), cancel.clone(), client));
            tokio::time::timeout(Duration::from_secs(2), mock.held.notified())
                .await
                .unwrap();
            let captured = load_active(&state).unwrap().unwrap();
            if expired {
                state
                    .db(|db| {
                        db.execute(
                            "UPDATE weixin_login_jobs SET expires_at=?1 WHERE id=?2",
                            params![now() - 1, created.id],
                        )?;
                        Ok(())
                    })
                    .unwrap();
            } else {
                let current = view(&state, &created.id);
                state
                    .db(|db| Ok(cancel_login(db, "admin", &created.id, current.revision)))
                    .unwrap()
                    .unwrap();
            }
            notify(&state);
            mock.release.notify_one();
            let done = wait(
                &state,
                &created.id,
                if expired { "expired" } else { "cancelled" },
            )
            .await;
            assert!(done.qr_content.is_none());
            assert!(!save_credentials(&state, &captured, credentials()).unwrap());
            assert!(state
                .db(|db| Ok(weixin_store::snapshot(db)?))
                .unwrap()
                .is_none());
            stop(cancel, worker, http).await;
        }
    }

    #[tokio::test]
    async fn account_revision_change_cannot_be_overwritten_or_enabled_by_late_login() {
        let (_temp, state) = open();
        let original = configured(&state, true);
        let (client, mock, http) = server(vec![(confirmed(), true)]).await;
        let created = start(&state, original.revision);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(run_login_with_client(state.clone(), cancel.clone(), client));
        tokio::time::timeout(Duration::from_secs(2), mock.held.notified())
            .await
            .unwrap();
        let captured = load_active(&state).unwrap().unwrap();
        state
            .db(|db| {
                Ok(weixin_store::set_enabled(
                    db,
                    original.revision,
                    "admin",
                    false,
                ))
            })
            .unwrap()
            .unwrap();
        notify(&state);
        mock.release.notify_one();
        wait(&state, &created.id, "failed").await;
        assert!(!save_credentials(&state, &captured, credentials()).unwrap());
        let actual = state
            .db(|db| Ok(weixin_store::snapshot(db)?))
            .unwrap()
            .unwrap();
        assert_eq!(actual.revision, original.revision + 1);
        assert!(!actual.enabled);
        assert_eq!(actual.credential_ref, original.credential_ref);
        stop(cancel, worker, http).await;
    }

    #[tokio::test]
    async fn protocol_terminal_and_existing_credential_states_are_honest_and_do_not_leak_qr() {
        for (response, existing, file, wanted) in [
            (json!({"status":"expired"}), false, false, "expired"),
            (
                json!({"status":"verify_code_blocked"}),
                false,
                false,
                "verification_blocked",
            ),
            (
                json!({"status":"scaned_but_redirect","redirect_host":"localhost"}),
                false,
                false,
                "failed",
            ),
            (
                json!({"status":"confirmed","ilink_bot_id":"bot@im.bot"}),
                false,
                false,
                "failed",
            ),
            (json!({"status":"binded_redirect"}), false, false, "failed"),
            (
                json!({"status":"binded_redirect","ilink_bot_id":"existing@im.bot"}),
                true,
                false,
                "failed",
            ),
            (
                json!({"status":"binded_redirect","ilink_bot_id":"other@im.bot"}),
                true,
                true,
                "failed",
            ),
            (
                json!({"status":"binded_redirect","ilink_bot_id":"existing@im.bot"}),
                true,
                true,
                "existing_credentials",
            ),
        ] {
            let (_temp, state) = open();
            let expected = if existing {
                configured(&state, file).revision
            } else {
                0
            };
            let (client, _mock, http) = server(vec![(response, false)]).await;
            let created = start(&state, expected);
            let cancel = CancellationToken::new();
            let worker = tokio::spawn(run_login_with_client(state.clone(), cancel.clone(), client));
            let result = wait(&state, &created.id, wanted).await;
            assert!(result.qr_content.is_none());
            let account = state.db(|db| Ok(weixin_store::snapshot(db)?)).unwrap();
            assert_eq!(account.is_some(), existing);
            if let Some(account) = account {
                assert_eq!(account.revision, expected);
                assert!(!account.enabled);
                assert_eq!(account.connection_status, "disabled");
            }
            stop(cancel, worker, http).await;
        }
    }

    #[test]
    fn redirect_budget_revision_owner_restart_and_single_job_constraints_are_persisted() {
        let (_temp, state) = open();
        let first = start(&state, 0);
        assert!(state
            .db(|db| Ok(start_login(db, "other-admin", 0)))
            .unwrap()
            .is_err());
        assert_eq!(
            state
                .db(|db| Ok(login_view(db, "other-admin", &first.id)))
                .unwrap()
                .err()
                .unwrap()
                .status,
            StatusCode::NOT_FOUND
        );
        for _ in 0..3 {
            let job = load_active(&state).unwrap().unwrap();
            apply_status(
                &state,
                &job,
                QrStatus::Redirect(
                    ApiBase::trusted_https("https://ilinkai2.weixin.qq.com/").unwrap(),
                ),
            )
            .unwrap();
            assert_eq!(view(&state, &first.id).status, "redirecting");
        }
        let job = load_active(&state).unwrap().unwrap();
        apply_status(
            &state,
            &job,
            QrStatus::Redirect(ApiBase::trusted_https("https://ilinkai3.weixin.qq.com/").unwrap()),
        )
        .unwrap();
        assert_eq!(
            view(&state, &first.id).error_code.as_deref(),
            Some("redirect_limit")
        );
        let second = start(&state, 0);
        state.db(|db| {db.execute("UPDATE weixin_login_jobs SET status='need_verification',qrcode='private-id',display_content='private-display',verification_code='123456' WHERE id=?1",[&second.id])?;initialize(db)?;Ok(())}).unwrap();
        let interrupted = view(&state, &second.id);
        assert_eq!(interrupted.status, "interrupted");
        assert!(interrupted.qr_content.is_none());
        for _ in 0..35 {
            let job = start(&state, 0);
            state
                .db(|db| Ok(cancel_login(db, "admin", &job.id, job.revision)))
                .unwrap()
                .unwrap();
        }
        state.db(|db| {maintenance(db)?;assert!(db.query_row("SELECT count(*) FROM weixin_login_jobs",[],|r|r.get::<_,i64>(0))?<=32);assert_eq!(db.query_row("SELECT count(*) FROM weixin_login_jobs WHERE qrcode IS NOT NULL OR verification_code IS NOT NULL",[],|r|r.get::<_,i64>(0))?,0);Ok(())}).unwrap();
    }

    #[test]
    fn receiver_check_results_require_the_same_enabled_account_generation_and_expire() {
        let (_temp, state) = open();
        let old = configured(&state, true);
        let account = state
            .db(|db| Ok(weixin_store::set_enabled(db, old.revision, "admin", true)))
            .unwrap()
            .unwrap();
        let request = state
            .db(|db| Ok(start_check(db, "admin", account.revision)))
            .unwrap()
            .unwrap();
        assert_eq!(request.status, "pending");
        assert!(state
            .db(|db| Ok(start_check(db, "admin", account.revision)))
            .unwrap()
            .is_err());
        state
            .db(|db| {
                assert!(complete_check(db, &account, CheckOutcome::Passed)?);
                assert!(pending_check(db)?.is_none());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            state
                .db(|db| latest_check(db, "admin"))
                .unwrap()
                .unwrap()
                .status,
            "passed"
        );
        state
            .db(|db| Ok(start_check(db, "admin", account.revision)))
            .unwrap()
            .unwrap();
        state
            .db(|db| {
                db.execute(
                    "UPDATE weixin_checks SET requested_at=?1 WHERE status='pending'",
                    [now() - 46],
                )?;
                assert!(pending_check(db)?.is_none());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            state
                .db(|db| latest_check(db, "admin"))
                .unwrap()
                .unwrap()
                .error_code
                .as_deref(),
            Some("check_expired")
        );
        state
            .db(|db| Ok(start_check(db, "admin", account.revision)))
            .unwrap()
            .unwrap();
        state
            .db(|db| {
                weixin_store::set_enabled(db, account.revision, "admin", false)?;
                assert!(!complete_check(db, &account, CheckOutcome::Passed)?);
                assert!(pending_check(db)?.is_none());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            state
                .db(|db| latest_check(db, "admin"))
                .unwrap()
                .unwrap()
                .error_code
                .as_deref(),
            Some("account_changed")
        );
    }

    #[test]
    fn already_bound_cannot_reuse_a_locally_known_invalid_token_even_if_its_file_exists() {
        let (_temporary, state) = open();
        let original = configured(&state, true);
        let invalid = state
            .db(|db| Ok(weixin_store::invalidate_token(db, &original)))
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(invalid.connection_status, "needs_login");
        assert!(state
            .secret(&invalid.credential_ref, "weixin_bot_token")
            .is_ok());
        let created = start(&state, invalid.revision);
        let job = load_active(&state).unwrap().unwrap();
        apply_status(
            &state,
            &job,
            QrStatus::AlreadyBound {
                account_id: Some(invalid.account_id),
            },
        )
        .unwrap();
        assert_eq!(view(&state, &created.id).status, "failed");
        assert_eq!(
            view(&state, &created.id).error_code.as_deref(),
            Some("existing_credentials_unavailable")
        );
        assert_eq!(
            state
                .db(|db| Ok(weixin_store::snapshot(db)?))
                .unwrap()
                .unwrap()
                .connection_status,
            "needs_login"
        );
    }

    #[tokio::test]
    async fn stopped_login_worker_scrubs_pending_qr_and_verification_without_dispatching() {
        let (_temporary, state) = open();
        let created = start(&state, 0);
        state.db(|db|{db.execute("UPDATE weixin_login_jobs SET qrcode='private-id',display_content='private-display',verification_code='123456' WHERE id=?1",[&created.id])?;Ok(())}).unwrap();
        let (client, mock, http) = server(Vec::new()).await;
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        run_login_with_client(state.clone(), cancelled, client).await;
        let current = view(&state, &created.id);
        assert_eq!(current.status, "interrupted");
        assert!(current.qr_content.is_none());
        assert_eq!(mock.fetches.load(Ordering::SeqCst), 0);
        assert!(mock.calls.lock().unwrap().is_empty());
        state.db(|db|{assert_eq!(db.query_row("SELECT count(*) FROM weixin_login_jobs WHERE qrcode IS NOT NULL OR display_content IS NOT NULL OR verification_code IS NOT NULL",[],|r|r.get::<_,i64>(0))?,0);Ok(())}).unwrap();
        http.abort();
    }
}
