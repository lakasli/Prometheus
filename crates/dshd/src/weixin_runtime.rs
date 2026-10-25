//! One managed receiver and sender for the single configured iLink account.
//! Neither loop launches per-conversation tasks or retries ambiguous sends.
use crate::{
    character,
    state::AppState,
    weixin_login::{self, CheckOutcome},
    weixin_protocol::{
        ApiBase, Cursor, InboundDisposition, ProtocolError, QuarantineReason, SecretToken,
        TextReply, UnsupportedReason, Updates, WeixinClient,
    },
    weixin_store::{self, AccountSnapshot, InboundEvent, PendingSend, SendOutcome, StoreError},
    worker,
};
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const IDLE_INTERVAL: Duration = Duration::from_secs(1);
const SEND_IDLE_INTERVAL: Duration = Duration::from_millis(500);
const SUCCESS_POLL_FLOOR: Duration = Duration::from_millis(250);

#[derive(Clone)]
struct ChannelClient {
    protocol: WeixinClient,
    #[cfg(test)]
    override_base: Option<ApiBase>,
}
impl ChannelClient {
    fn production() -> Self {
        Self {
            protocol: WeixinClient::production(),
            #[cfg(test)]
            override_base: None,
        }
    }
    fn base(&self, value: &str) -> std::result::Result<ApiBase, ProtocolError> {
        #[cfg(test)]
        if let Some(base) = &self.override_base {
            return Ok(base.clone());
        }
        ApiBase::trusted_https(value)
    }
    #[cfg(test)]
    fn for_test(base: &str) -> Self {
        let (protocol, base) = WeixinClient::for_test(base, true).unwrap();
        Self {
            protocol,
            override_base: Some(base),
        }
    }
}

fn signal(state: &AppState) {
    state
        .0
        .weixin_signal
        .send_modify(|value| *value = value.wrapping_add(1));
    state.0.notify.notify_waiters();
}
fn snapshot(state: &AppState) -> weixin_store::Result<Option<AccountSnapshot>> {
    state
        .db(|db| Ok(weixin_store::snapshot(db)))
        .map_err(|_| StoreError::Storage)?
}
fn token(state: &AppState, credential: &str) -> std::result::Result<SecretToken, ()> {
    let value = state
        .secret(credential, "weixin_bot_token")
        .map_err(|_| ())?;
    SecretToken::new(value).map_err(|_| ())
}
fn status(state: &AppState, current: &AccountSnapshot, value: &str, code: Option<&str>) {
    let _ = state.db(|db| Ok(weixin_store::record_status(db, current, value, code)));
}
fn require_login(state: &AppState, current: &AccountSnapshot, code: &str) {
    let changed = state.db(|db| Ok(weixin_store::require_login(db, current, code)));
    if matches!(changed, Ok(Ok(Some(_)))) {
        signal(state);
    }
}
fn finish_check(state: &AppState, current: &AccountSnapshot, outcome: CheckOutcome) {
    let _ = state.db(|db| {
        if weixin_login::pending_check(db)?.is_some_and(|check| {
            check.account_id == current.account_id
                && check.generation == current.generation
                && check.revision == current.revision
        }) {
            weixin_login::complete_check(db, current, outcome)?;
        }
        Ok(())
    });
}
fn check_failure(error: &ProtocolError) -> &'static str {
    match error {
        error if error.is_stale_token() => "token_expired",
        ProtocolError::UntrustedEndpoint => "untrusted_endpoint",
        ProtocolError::PrivateAddress => "private_address",
        ProtocolError::Dns => "dns_error",
        ProtocolError::Http(_) => "http_error",
        ProtocolError::Business { .. } => "business_error",
        ProtocolError::Transport => "transport_error",
        ProtocolError::Timeout => "long_poll_timeout",
        ProtocolError::Cancelled => "cancelled",
        _ => "protocol_error",
    }
}
fn ambiguous_send(error: &ProtocolError) -> bool {
    matches!(
        error,
        ProtocolError::Transport
            | ProtocolError::Timeout
            | ProtocolError::Cancelled
            | ProtocolError::InvalidResponse(_)
            | ProtocolError::LimitExceeded("response_body")
            | ProtocolError::Http(500..=599)
    )
}
fn protocol_code(error: &ProtocolError) -> String {
    match error {
        ProtocolError::InvalidInput(_) => "invalid_input".into(),
        ProtocolError::InvalidResponse(_) => "invalid_response".into(),
        ProtocolError::LimitExceeded(_) => "response_limit".into(),
        ProtocolError::UntrustedEndpoint => "untrusted_endpoint".into(),
        ProtocolError::PrivateAddress => "private_address".into(),
        ProtocolError::Dns => "dns_error".into(),
        ProtocolError::Http(code) => format!("http_{code}"),
        ProtocolError::Business { ret, errcode } => {
            format!("business_{}_{}", ret.unwrap_or(0), errcode.unwrap_or(0))
        }
        ProtocolError::Transport => "transport_error".into(),
        ProtocolError::Timeout => "request_timeout".into(),
        ProtocolError::Cancelled => "request_cancelled".into(),
    }
}
fn store_code(error: &StoreError) -> &'static str {
    match error {
        StoreError::Capacity => "queue_capacity",
        StoreError::EnqueueFailed => "run_enqueue_rejected",
        StoreError::Invalid(_) => "invalid_batch",
        StoreError::Storage => "storage_error",
        StoreError::Conflict => "revision_conflict",
        StoreError::NotConfigured => "not_configured",
        StoreError::Disabled => "disabled",
        StoreError::StaleGeneration => "stale_generation",
        StoreError::StaleCursor => "stale_cursor",
    }
}
fn unsupported_code(reason: &UnsupportedReason) -> &'static str {
    match reason {
        UnsupportedReason::Group => "group_unsupported",
        UnsupportedReason::BotMessage => "bot_message",
        UnsupportedReason::UnknownMessageType => "unknown_message_type",
        UnsupportedReason::NotFinished => "not_finished",
        UnsupportedReason::Deleted => "deleted_message",
        UnsupportedReason::MediaOrUnknownItem => "media_unsupported",
        UnsupportedReason::EmptyText => "empty_text",
    }
}
fn quarantine_code(reason: &QuarantineReason) -> &'static str {
    match reason {
        QuarantineReason::MalformedEvent => "malformed_event",
        QuarantineReason::MissingMessageId => "missing_message_id",
        QuarantineReason::InvalidMessageId => "invalid_message_id",
        QuarantineReason::MissingSender => "missing_sender",
        QuarantineReason::MissingRecipient => "missing_recipient",
        QuarantineReason::MissingContext => "missing_context",
        QuarantineReason::InvalidContext => "invalid_context",
        QuarantineReason::InputTooLarge => "input_limit",
    }
}
fn rejected(
    message_id: Option<String>,
    reason: &str,
    cursor_hash: &str,
    index: usize,
) -> InboundEvent {
    // Stable IDs remain stable across replayed cursors. Without an ID, only
    // opaque cursor hash and batch position are available; never hash a token
    // or the text merely to turn a diagnostic into hidden sensitive storage.
    let key = if let Some(id) = &message_id {
        format!("id:{id}:reason:{reason}")
    } else {
        format!("cursor:{cursor_hash}:index:{index}:reason:{reason}")
    };
    InboundEvent::Rejected {
        message_id,
        reason: reason.into(),
        fingerprint: hex::encode(Sha256::digest(key.as_bytes())),
    }
}

fn convert_batch(
    account: &AccountSnapshot,
    updates: Updates,
) -> (String, Vec<InboundEvent>, Duration) {
    let cursor_hash = hex::encode(Sha256::digest(account.cursor.as_bytes()));
    let events = updates
        .events
        .into_iter()
        .enumerate()
        .map(|(index, event)| match event {
            InboundDisposition::Text(text) if text.to_user_id != account.account_id => rejected(
                Some(text.message_id),
                "wrong_recipient",
                &cursor_hash,
                index,
            ),
            InboundDisposition::Text(text) if text.from_user_id != account.linked_user_id => {
                rejected(
                    Some(text.message_id),
                    "unauthorized_peer",
                    &cursor_hash,
                    index,
                )
            }
            InboundDisposition::Text(text) if text.text.len() > 16000 => {
                rejected(Some(text.message_id), "input_limit", &cursor_hash, index)
            }
            InboundDisposition::Text(text) => InboundEvent::Text {
                message_id: text.message_id,
                peer: text.from_user_id,
                text: text.text,
                context_token: text.context_token,
            },
            InboundDisposition::Unsupported(event) => rejected(
                event.message_id,
                unsupported_code(&event.reason),
                &cursor_hash,
                index,
            ),
            InboundDisposition::Quarantine(event) => rejected(
                event.message_id,
                quarantine_code(&event.reason),
                &cursor_hash,
                index,
            ),
        })
        .collect();
    let cursor = updates
        .next_cursor
        .map(|c| c.expose().to_owned())
        .unwrap_or_else(|| account.cursor.clone());
    (cursor, events, updates.next_timeout)
}

fn enqueue(
    db: &rusqlite::Connection,
    conversation: &str,
    owner: &str,
    client_id: &str,
    input: &str,
) -> anyhow::Result<String> {
    // A control command must remain usable without a model or valid preferred
    // role mapping. The first ordinary message binds the then-current default.
    if !crate::memory_commands::is_command(input) {
        let has_binding = db
            .query_row(
                "SELECT 1 FROM conversation_character_bindings WHERE conversation_id=?1",
                [conversation],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        let has_model_work = db
            .query_row(
                "SELECT 1 FROM runs WHERE conversation_id=?1 AND run_kind IN ('model','skill_text','skill_image') LIMIT 1",
                [conversation],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if !has_binding && !has_model_work {
            let (_, config) = character::current_config_in(db)?;
            character::bind_new_conversation(db, conversation, None, &config)?;
        }
    }
    worker::enqueue_in(db, conversation, owner, client_id, input, None)?
        .ok_or_else(|| anyhow::anyhow!("Weixin run admission rejected"))
}

async fn wait(
    _state: &AppState,
    changed: &mut watch::Receiver<u64>,
    cancel: &CancellationToken,
    duration: Duration,
) -> bool {
    tokio::select! {
        biased;
        _=cancel.cancelled()=>false,
        result=changed.changed()=>result.is_ok(),
        _=tokio::time::sleep(duration)=>true,
    }
}

async fn wait_for_work(
    state: &AppState,
    changed: &mut watch::Receiver<u64>,
    cancel: &CancellationToken,
    duration: Duration,
) -> bool {
    tokio::select! {
        biased;
        result=wait(state,changed,cancel,duration)=>result,
        _=state.0.notify.notified()=>true,
    }
}

pub async fn run_receiver(state: AppState, cancel: CancellationToken) {
    run_receiver_with(state, cancel, ChannelClient::production()).await;
}
async fn run_receiver_with(state: AppState, cancel: CancellationToken, client: ChannelClient) {
    // Register before reading any account. A change between the read and HTTP
    // request is already pending when select polls changed().
    let mut changed = state.0.weixin_signal.subscribe();
    let mut failures = 0u32;
    let mut timeout = None;
    let mut active_generation: Option<String> = None;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        changed.borrow_and_update();
        // Also expire checks while disabled or idle; this only touches bounded
        // operation metadata and never creates another network poller.
        let _ = state.db(|db| {
            weixin_login::pending_check(db)?;
            Ok(())
        });
        let current = match snapshot(&state) {
            Ok(Some(current)) if current.enabled => current,
            Ok(_) => {
                if !wait(&state, &mut changed, &cancel, IDLE_INTERVAL).await {
                    break;
                }
                continue;
            }
            Err(_) => {
                tracing::warn!(
                    channel = "weixin",
                    stage = "receive",
                    code = "storage_error",
                    "Weixin state unavailable"
                );
                if !wait(&state, &mut changed, &cancel, IDLE_INTERVAL).await {
                    break;
                }
                continue;
            }
        };
        if active_generation.as_deref() != Some(current.generation.as_str()) {
            active_generation = Some(current.generation.clone());
            failures = 0;
            timeout = None;
        }
        let token = match token(&state, &current.credential_ref) {
            Ok(token) => token,
            Err(()) => {
                finish_check(
                    &state,
                    &current,
                    CheckOutcome::Failed("credential_unavailable"),
                );
                require_login(&state, &current, "credential_unavailable");
                continue;
            }
        };
        let base = match client.base(&current.base_url) {
            Ok(base) => base,
            Err(_) => {
                finish_check(&state, &current, CheckOutcome::Failed("untrusted_endpoint"));
                require_login(&state, &current, "untrusted_endpoint");
                continue;
            }
        };
        let cursor = if current.cursor.is_empty() {
            None
        } else {
            match Cursor::new(current.cursor.clone()) {
                Ok(cursor) => Some(cursor),
                Err(_) => {
                    finish_check(&state, &current, CheckOutcome::Failed("invalid_cursor"));
                    require_login(&state, &current, "invalid_cursor");
                    continue;
                }
            }
        };
        let request_cancel = cancel.child_token();
        let outcome = tokio::select! {
            biased;
            _=cancel.cancelled()=>break,
            result=changed.changed()=>{request_cancel.cancel();if result.is_err(){break;}timeout=None;failures=0;continue;},
            outcome=client.protocol.get_updates(&base,&token,cursor.as_ref(),timeout,&request_cancel)=>outcome,
        };
        match outcome {
            Ok(updates) => {
                finish_check(&state, &current, CheckOutcome::Passed);
                let (cursor, events, next_timeout) = convert_batch(&current, updates);
                let result = state.db(|db| {
                    Ok(weixin_store::ingest_batch(
                        db, &current, &cursor, &events, enqueue,
                    ))
                });
                match result {
                    Ok(Ok(result)) => {
                        failures = 0;
                        timeout = Some(next_timeout);
                        if result.accepted > 0 {
                            state.0.notify.notify_waiters();
                        }
                        if !wait(&state, &mut changed, &cancel, SUCCESS_POLL_FLOOR).await {
                            break;
                        }
                    }
                    Ok(Err(
                        StoreError::StaleGeneration
                        | StoreError::StaleCursor
                        | StoreError::Disabled
                        | StoreError::NotConfigured,
                    )) => {
                        failures = 0;
                        timeout = None;
                    }
                    error => {
                        let code = match &error {
                            Ok(Err(error)) => store_code(error),
                            _ => "storage_error",
                        };
                        status(&state, &current, "backoff", Some(code));
                        failures = failures.saturating_add(1).min(7);
                        if !wait(&state, &mut changed, &cancel, backoff(failures)).await {
                            break;
                        }
                    }
                }
            }
            Err(ProtocolError::Timeout) => {
                finish_check(&state, &current, CheckOutcome::Failed("long_poll_timeout"));
                // No fabricated ret=0 or cursor acknowledgement. Normal server
                // long-poll timeout simply starts another cancellable poll.
                failures = 0;
                if !wait(&state, &mut changed, &cancel, SUCCESS_POLL_FLOOR).await {
                    break;
                }
            }
            Err(error) if error.is_stale_token() => {
                finish_check(&state, &current, CheckOutcome::Failed("token_expired"));
                require_login(&state, &current, "token_expired");
                failures = 0;
                timeout = None;
            }
            Err(ProtocolError::Cancelled) if cancel.is_cancelled() => break,
            Err(error) => {
                finish_check(
                    &state,
                    &current,
                    CheckOutcome::Failed(check_failure(&error)),
                );
                let code = protocol_code(&error);
                status(&state, &current, "backoff", Some(&code));
                tracing::warn!(channel="weixin",stage="receive",code=%code,"Weixin receive deferred");
                failures = failures.saturating_add(1).min(7);
                if !wait(&state, &mut changed, &cancel, backoff(failures)).await {
                    break;
                }
            }
        }
    }
}
fn backoff(failures: u32) -> Duration {
    Duration::from_secs((1u64 << failures.saturating_sub(1).min(6)).min(60))
}

pub async fn run_sender(state: AppState, cancel: CancellationToken) {
    run_sender_with(state, cancel, ChannelClient::production()).await;
}
async fn run_sender_with(state: AppState, cancel: CancellationToken, client: ChannelClient) {
    let mut changed = state.0.weixin_signal.subscribe();
    loop {
        if cancel.is_cancelled() {
            break;
        }
        changed.borrow_and_update();
        let pending = match state.db(|db| Ok(weixin_store::claim_send(db))) {
            Ok(Ok(Some(pending))) => pending,
            Ok(Ok(None)) => {
                if !wait_for_work(&state, &mut changed, &cancel, SEND_IDLE_INTERVAL).await {
                    break;
                }
                continue;
            }
            _ => {
                tracing::warn!(
                    channel = "weixin",
                    stage = "send",
                    code = "storage_error",
                    "Weixin send queue unavailable"
                );
                if !wait(&state, &mut changed, &cancel, IDLE_INTERVAL).await {
                    break;
                }
                continue;
            }
        };
        send_one(&state, &pending, &mut changed, &cancel, &client).await;
    }
}

async fn send_one(
    state: &AppState,
    pending: &PendingSend,
    changed: &mut watch::Receiver<u64>,
    cancel: &CancellationToken,
    client: &ChannelClient,
) {
    let context = match state.db(|db| Ok(weixin_store::load_context(db, pending))) {
        Ok(Ok(context)) => context,
        _ => {
            complete(
                state,
                pending,
                SendOutcome::Unknown {
                    code: "context_unavailable".into(),
                },
            );
            return;
        }
    };
    let token = match token(state, &pending.credential_ref) {
        Ok(token) => token,
        Err(()) => {
            complete(
                state,
                pending,
                SendOutcome::Failed {
                    code: "credential_unavailable".into(),
                },
            );
            if let Ok(Some(current)) = snapshot(state) {
                if current.generation == pending.generation {
                    require_login(state, &current, "credential_unavailable");
                }
            }
            return;
        }
    };
    let base = match client.base(&pending.base_url) {
        Ok(base) => base,
        Err(_) => {
            complete(
                state,
                pending,
                SendOutcome::Failed {
                    code: "untrusted_endpoint".into(),
                },
            );
            return;
        }
    };
    let reply = TextReply {
        to_user_id: pending.peer.clone(),
        client_id: pending.client_id.clone(),
        text: pending.content.clone(),
        context_token: context,
        run_id: Some(pending.run_id.clone()),
    };
    let request_cancel = cancel.child_token();
    let result = tokio::select! {
        biased;
        _=cancel.cancelled()=>{request_cancel.cancel();Err(ProtocolError::Cancelled)},
        _=changed.changed()=>{request_cancel.cancel();Err(ProtocolError::Cancelled)},
        result=client.protocol.send_text(&base,&token,&reply,&request_cancel)=>result,
    };
    match result {
        Ok(accepted) => complete(
            state,
            pending,
            SendOutcome::Accepted {
                server_message_id: accepted.server_message_id,
            },
        ),
        Err(error) => {
            let code = protocol_code(&error);
            let outcome = if ambiguous_send(&error) {
                SendOutcome::Unknown { code: code.clone() }
            } else {
                SendOutcome::Failed { code: code.clone() }
            };
            complete(state, pending, outcome);
            if error.is_stale_token() {
                if let Ok(Some(current)) = snapshot(state) {
                    if current.generation == pending.generation {
                        require_login(state, &current, "token_expired");
                    }
                }
            }
            tracing::warn!(channel="weixin",stage="send",code=%code,"Weixin send did not obtain acceptance");
        }
    }
}
fn complete(state: &AppState, pending: &PendingSend, outcome: SendOutcome) {
    if !matches!(
        state.db(|db| Ok(weixin_store::complete_send(db, pending, outcome))),
        Ok(Ok(_))
    ) {
        // A failed durable completion leaves sending, which startup marks
        // unknown. Never force another transmission because persistence failed.
        tracing::error!(
            channel = "weixin",
            stage = "send",
            code = "completion_persist_failed",
            "Weixin send outcome not persisted"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::RuntimeConfig,
        state::{now, private_write, Options},
        weixin_store::AccountInput,
    };
    use axum::{
        body::{to_bytes, Body},
        extract::State,
        http::{Request, StatusCode},
        routing::post,
        Json, Router,
    };
    use rusqlite::params;
    use serde_json::{json, Value};
    use std::{
        collections::VecDeque,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };
    use tempfile::TempDir;
    use tokio::{sync::Notify, task::JoinHandle};
    use tower::ServiceExt;

    struct Mock {
        updates: Mutex<VecDeque<Value>>,
        received: Mutex<Vec<Value>>,
        sent: Mutex<Vec<Value>>,
        model_inputs: Mutex<Vec<Value>>,
        polls: AtomicUsize,
        sends: AtomicUsize,
        poll_entered: Notify,
        send_entered: Notify,
        hold_send: AtomicBool,
        send_response: Mutex<Value>,
        release_send: CancellationToken,
        cancel: CancellationToken,
    }
    impl Mock {
        fn new(updates: Vec<Value>) -> Arc<Self> {
            Arc::new(Self {
                updates: Mutex::new(updates.into()),
                received: Mutex::new(Vec::new()),
                sent: Mutex::new(Vec::new()),
                model_inputs: Mutex::new(Vec::new()),
                polls: AtomicUsize::new(0),
                sends: AtomicUsize::new(0),
                poll_entered: Notify::new(),
                send_entered: Notify::new(),
                hold_send: AtomicBool::new(false),
                send_response: Mutex::new(json!({"ret":0,"message_id":u64::MAX})),
                release_send: CancellationToken::new(),
                cancel: CancellationToken::new(),
            })
        }
    }
    struct HttpFixture {
        base: String,
        data: Arc<Mock>,
        server: JoinHandle<()>,
    }
    impl HttpFixture {
        async fn start(updates: Vec<Value>) -> Self {
            async fn poll(State(mock): State<Arc<Mock>>, Json(body): Json<Value>) -> Json<Value> {
                mock.polls.fetch_add(1, Ordering::SeqCst);
                mock.received.lock().unwrap().push(body);
                mock.poll_entered.notify_one();
                let next = { mock.updates.lock().unwrap().pop_front() };
                if let Some(next) = next {
                    return Json(next);
                }
                tokio::select! {_=mock.cancel.cancelled()=>{},_=tokio::time::sleep(Duration::from_secs(30))=>{}}
                Json(json!({"ret":0,"msgs":[]}))
            }
            async fn send(State(mock): State<Arc<Mock>>, Json(body): Json<Value>) -> Json<Value> {
                mock.sends.fetch_add(1, Ordering::SeqCst);
                mock.sent.lock().unwrap().push(body);
                mock.send_entered.notify_one();
                if mock.hold_send.load(Ordering::SeqCst) {
                    tokio::select! {_=mock.release_send.cancelled()=>{},_=mock.cancel.cancelled()=>{}}
                }
                Json(mock.send_response.lock().unwrap().clone())
            }
            async fn model(State(mock): State<Arc<Mock>>, Json(body): Json<Value>) -> Json<Value> {
                mock.model_inputs.lock().unwrap().push(body);
                Json(
                    json!({"choices":[{"message":{"role":"assistant","content":"native mock reply"}}]}),
                )
            }
            let data = Mock::new(updates);
            let app = Router::new()
                .route("/ilink/bot/getupdates", post(poll))
                .route("/ilink/bot/sendmessage", post(send))
                .route("/chat/completions", post(model))
                .with_state(data.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let cancel = data.cancel.clone();
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(cancel.cancelled_owned())
                    .await;
            });
            Self { base, data, server }
        }
        fn client(&self) -> ChannelClient {
            ChannelClient::for_test(&self.base)
        }
        async fn close(self) {
            self.data.cancel.cancel();
            tokio::time::timeout(Duration::from_secs(3), self.server)
                .await
                .unwrap()
                .unwrap();
        }
    }
    fn fixture(base: &str) -> (TempDir, AppState) {
        let directory = TempDir::new().unwrap();
        let state = AppState::open(Options {
            data_dir: directory.path().to_owned(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: directory.path().join("web"),
        })
        .unwrap();
        state
            .db(|db| {
                weixin_store::initialize(db)?;
                Ok(())
            })
            .unwrap();
        let credential = uuid::Uuid::new_v4().to_string();
        let model = uuid::Uuid::new_v4().to_string();
        private_write(
            &directory.path().join("secrets").join(&credential),
            b"synthetic-weixin-token",
        )
        .unwrap();
        private_write(
            &directory.path().join("secrets").join(&model),
            b"synthetic-model-key",
        )
        .unwrap();
        state
            .db(|db| {
                db.execute(
                    "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'weixin_bot_token',?2)",
                    params![credential, now()],
                )?;
                db.execute(
                    "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'model_api_key',?2)",
                    params![model, now()],
                )?;
                weixin_store::replace_account(
                    db,
                    0,
                    "admin",
                    &AccountInput {
                        account_id: "bot@im.bot".into(),
                        base_url: "https://ilinkai.weixin.qq.com/".into(),
                        credential_ref: credential,
                        linked_user_id: "owner@im.wechat".into(),
                        enabled: true,
                    },
                )?;
                let mut config = RuntimeConfig::default();
                config.model.base_url = base.into();
                config.model.model = "loopback-model".into();
                config.model.credential_ref = Some(model);
                config.assistant.timeout_seconds = 10;
                db.execute(
                    "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                    [serde_json::to_string(&config)?],
                )?;
                Ok(())
            })
            .unwrap();
        (directory, state)
    }
    fn message(id: u64, text: &str) -> Value {
        json!({"message_id":id,"from_user_id":"owner@im.wechat","to_user_id":"bot@im.bot","message_type":1,"message_state":2,"context_token":format!("context-{id}"),"item_list":[{"type":1,"text_item":{"text":text}}]})
    }
    fn batch(events: Vec<Value>, cursor: &str) -> Value {
        json!({"ret":0,"msgs":events,"get_updates_buf":cursor})
    }
    fn scalar(state: &AppState, sql: &str) -> i64 {
        state
            .db(|db| Ok(db.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }
    async fn until(mut predicate: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(6), async {
            while !predicate() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn stop(cancel: CancellationToken, tasks: Vec<JoinHandle<()>>) {
        cancel.cancel();
        for task in tasks {
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn real_http_receive_native_model_and_frozen_context_send_form_one_durable_pipeline() {
        let http = HttpFixture::start(vec![
            batch(
                vec![message(u64::MAX, "hello local fixture")],
                "cursor-next",
            ),
            batch(
                vec![message(u64::MAX, "hello local fixture")],
                "cursor-replay",
            ),
        ])
        .await;
        let (_directory, state) = fixture(&http.base);
        let cancel = CancellationToken::new();
        let receiver = tokio::spawn(run_receiver_with(
            state.clone(),
            cancel.clone(),
            http.client(),
        ));
        let native = tokio::spawn(worker::run_worker(state.clone(), cancel.clone()));
        let sender = tokio::spawn(run_sender_with(
            state.clone(),
            cancel.clone(),
            http.client(),
        ));
        until(|| {
            scalar(
                &state,
                "SELECT count(*) FROM weixin_outbox WHERE status='accepted'",
            ) == 1
                && snapshot(&state).unwrap().unwrap().cursor == "cursor-replay"
        })
        .await;
        assert_eq!(scalar(&state, "SELECT count(*) FROM runs"), 1);
        assert_eq!(scalar(&state, "SELECT count(*) FROM outbox"), 0);
        assert_eq!(http.data.sends.load(Ordering::SeqCst), 1);
        let sent = http.data.sent.lock().unwrap()[0].clone();
        assert_eq!(
            sent["msg"]["context_token"],
            format!("context-{}", u64::MAX)
        );
        assert_eq!(sent["msg"]["to_user_id"], "owner@im.wechat");
        assert_eq!(
            sent["msg"]["item_list"][0]["text_item"]["text"],
            "native mock reply"
        );
        {
            let inputs = http.data.model_inputs.lock().unwrap();
            assert_eq!(inputs.len(), 1);
            assert!(!inputs[0].to_string().contains("context_token"));
        }
        assert_eq!(
            scalar(
                &state,
                "SELECT count(*) FROM weixin_contexts WHERE token IS NOT NULL"
            ),
            0
        );
        stop(cancel, vec![receiver, native, sender]).await;
        http.close().await;
    }

    #[tokio::test]
    async fn real_http_control_reply_is_private_model_free_and_replay_idempotent() {
        let input = "/memory remember synthetic owner preference";
        let http = HttpFixture::start(vec![
            batch(vec![message(7, input)], "one"),
            batch(vec![message(7, input)], "two"),
        ])
        .await;
        let (_directory, state) = fixture(&http.base);
        let cancel = CancellationToken::new();
        let tasks = vec![
            tokio::spawn(run_receiver_with(
                state.clone(),
                cancel.clone(),
                http.client(),
            )),
            tokio::spawn(worker::run_worker(state.clone(), cancel.clone())),
            tokio::spawn(run_sender_with(
                state.clone(),
                cancel.clone(),
                http.client(),
            )),
        ];
        until(|| {
            scalar(
                &state,
                "SELECT count(*) FROM weixin_outbox WHERE status='accepted'",
            ) == 1
                && snapshot(&state).unwrap().unwrap().cursor == "two"
        })
        .await;
        assert_eq!(scalar(&state,"SELECT count(*) FROM memory_records WHERE visibility='private' AND source_owner_id='weixin:bot@im.bot:c2c:owner@im.wechat'"),1);
        assert_eq!(
            scalar(
                &state,
                "SELECT count(*) FROM runs WHERE run_kind='memory_command'"
            ),
            1
        );
        assert_eq!(http.data.model_inputs.lock().unwrap().len(), 0);
        assert_eq!(http.data.sends.load(Ordering::SeqCst), 1);
        stop(cancel, tasks).await;
        http.close().await;
    }

    #[tokio::test]
    async fn foreign_peers_recipients_media_groups_and_bad_ids_are_quarantined_before_model_work() {
        let mut peer = message(2, "not authorized");
        peer["from_user_id"] = json!("stranger@im.wechat");
        let mut recipient = message(3, "wrong bot");
        recipient["to_user_id"] = json!("other@im.bot");
        let mut group = message(4, "group");
        group["group_id"] = json!("group-fixture");
        let mut media = message(5, "media");
        media["item_list"] = json!([{"type":2,"image_item":{}}]);
        let mut malformed = message(6, "invalid id");
        malformed["message_id"] = json!("18446744073709551616");
        let oversized = message(8, &"x".repeat(16001));
        let http = HttpFixture::start(vec![batch(
            vec![peer, recipient, group, media, malformed, oversized],
            "rejected-cursor",
        )])
        .await;
        let (_directory, state) = fixture(&http.base);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_receiver_with(
            state.clone(),
            cancel.clone(),
            http.client(),
        ));
        until(|| snapshot(&state).unwrap().unwrap().cursor == "rejected-cursor").await;
        assert_eq!(scalar(&state, "SELECT count(*) FROM runs"), 0);
        assert_eq!(
            scalar(
                &state,
                "SELECT count(*) FROM weixin_inbox WHERE disposition='rejected'"
            ),
            6
        );
        assert_eq!(scalar(&state,"SELECT count(*) FROM weixin_inbox WHERE reason_code IN ('unauthorized_peer','wrong_recipient')"),2);
        assert_eq!(scalar(&state, "SELECT count(*) FROM weixin_contexts"), 0);
        stop(cancel, vec![task]).await;
        http.close().await;
    }

    #[tokio::test]
    async fn full_run_queue_rolls_back_entire_batch_and_cursor_then_recovers_on_signal() {
        let updates = batch(
            vec![message(42, "admitted after capacity frees")],
            "after-capacity",
        );
        let http = HttpFixture::start(vec![updates.clone(), updates]).await;
        let (_directory, state) = fixture(&http.base);
        state.db(|db|{
            let tx=db.transaction()?;let conversation=uuid::Uuid::new_v4().to_string();let owner="weixin:bot@im.bot:c2c:owner@im.wechat";
            tx.execute("INSERT INTO conversations(id,owner_id,title,source_key,created_at) VALUES(?1,?2,'full fixture',?2,0)",params![conversation,owner])?;
            for index in 0..10 {worker::enqueue_in(&tx,&conversation,owner,&format!("old-{index}"),"old queued fixture",None)?.unwrap();}
            tx.commit()?;Ok(())
        }).unwrap();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_receiver_with(
            state.clone(),
            cancel.clone(),
            http.client(),
        ));
        until(|| {
            snapshot(&state)
                .unwrap()
                .unwrap()
                .last_error_code
                .as_deref()
                == Some("run_enqueue_rejected")
        })
        .await;
        assert_eq!(snapshot(&state).unwrap().unwrap().cursor, "");
        assert_eq!(scalar(&state, "SELECT count(*) FROM weixin_inbox"), 0);
        state
            .db(|db| {
                db.execute(
                    "UPDATE runs SET status='cancelled' WHERE status='queued'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        signal(&state);
        until(|| snapshot(&state).unwrap().unwrap().cursor == "after-capacity").await;
        assert_eq!(
            scalar(
                &state,
                "SELECT count(*) FROM weixin_inbox WHERE disposition='accepted'"
            ),
            1
        );
        assert_eq!(
            scalar(&state, "SELECT count(*) FROM runs WHERE status='queued'"),
            1
        );
        stop(cancel, vec![task]).await;
        http.close().await;
    }

    #[tokio::test]
    async fn token_invalid_response_disables_generation_and_never_acknowledges_a_cursor() {
        let http=HttpFixture::start(vec![json!({"ret":-14,"errcode":-14,"errmsg":"must-not-log-provider-body","get_updates_buf":"unacknowledged"})]).await;
        let (_directory, state) = fixture(&http.base);
        let before = snapshot(&state).unwrap().unwrap();
        state
            .db(|db| Ok(weixin_login::start_check(db, "admin", before.revision)))
            .unwrap()
            .unwrap();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_receiver_with(
            state.clone(),
            cancel.clone(),
            http.client(),
        ));
        until(|| snapshot(&state).unwrap().unwrap().connection_status == "needs_login").await;
        let after = snapshot(&state).unwrap().unwrap();
        assert!(!after.enabled);
        assert_ne!(after.generation, before.generation);
        assert_eq!(after.cursor, "");
        assert_eq!(after.last_error_code.as_deref(), Some("token_expired"));
        assert_eq!(scalar(&state,"SELECT count(*) FROM weixin_checks WHERE status='failed' AND error_code='token_expired'"),1);
        signal(&state);
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(http.data.polls.load(Ordering::SeqCst), 1);
        stop(cancel, vec![task]).await;
        http.close().await;
    }

    #[tokio::test]
    async fn disable_cancels_witnessed_send_and_unknown_attempt_is_never_replayed() {
        let http = HttpFixture::start(vec![batch(
            vec![message(9, "native before disable")],
            "received",
        )])
        .await;
        http.data.hold_send.store(true, Ordering::SeqCst);
        let (_directory, state) = fixture(&http.base);
        let cancel = CancellationToken::new();
        let tasks = vec![
            tokio::spawn(run_receiver_with(
                state.clone(),
                cancel.clone(),
                http.client(),
            )),
            tokio::spawn(worker::run_worker(state.clone(), cancel.clone())),
            tokio::spawn(run_sender_with(
                state.clone(),
                cancel.clone(),
                http.client(),
            )),
        ];
        tokio::time::timeout(Duration::from_secs(5), http.data.send_entered.notified())
            .await
            .unwrap();
        let before = snapshot(&state).unwrap().unwrap();
        let disabled = state
            .db(|db| {
                Ok(weixin_store::set_enabled(
                    db,
                    before.revision,
                    "admin",
                    false,
                ))
            })
            .unwrap()
            .unwrap();
        signal(&state);
        until(|| {
            scalar(
                &state,
                "SELECT count(*) FROM weixin_outbox WHERE status='unknown'",
            ) == 1
        })
        .await;
        assert_eq!(http.data.sends.load(Ordering::SeqCst), 1);
        state
            .db(|db| {
                Ok(weixin_store::set_enabled(
                    db,
                    disabled.revision,
                    "admin",
                    true,
                ))
            })
            .unwrap()
            .unwrap();
        signal(&state);
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(http.data.sends.load(Ordering::SeqCst), 1);
        stop(cancel, tasks).await;
        http.close().await;
    }

    #[tokio::test]
    async fn cancel_interrupts_a_witnessed_long_poll_without_advancing_cursor() {
        let http = HttpFixture::start(Vec::new()).await;
        let (_directory, state) = fixture(&http.base);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_receiver_with(
            state.clone(),
            cancel.clone(),
            http.client(),
        ));
        tokio::time::timeout(Duration::from_secs(3), http.data.poll_entered.notified())
            .await
            .unwrap();
        stop(cancel, vec![task]).await;
        assert_eq!(snapshot(&state).unwrap().unwrap().cursor, "");
        assert_eq!(scalar(&state, "SELECT count(*) FROM weixin_inbox"), 0);
        http.close().await;
    }

    #[test]
    fn rejection_fingerprints_are_stable_for_ids_and_contain_no_text_or_context() {
        let first = rejected(Some("7".into()), "not_finished", "old-cursor", 0);
        let again = rejected(Some("7".into()), "not_finished", "new-cursor", 9);
        let (
            InboundEvent::Rejected {
                fingerprint: first, ..
            },
            InboundEvent::Rejected {
                fingerprint: again, ..
            },
        ) = (first, again)
        else {
            panic!("expected rejection")
        };
        assert_eq!(first, again);
        assert_eq!(first.len(), 64);
        assert_eq!(
            protocol_code(&ProtocolError::Business {
                ret: Some(-14),
                errcode: None
            }),
            "business_-14_0"
        );
        assert_eq!(backoff(99), Duration::from_secs(60));
        assert!(ambiguous_send(&ProtocolError::InvalidResponse(
            "missing_ret"
        )));
        assert!(ambiguous_send(&ProtocolError::Http(503)));
        assert!(!ambiguous_send(&ProtocolError::Business {
            ret: Some(5),
            errcode: None
        }));
        assert!(!ambiguous_send(&ProtocolError::InvalidInput("output_text")));
    }

    #[tokio::test]
    async fn pending_connection_check_uses_the_single_receiver_and_requires_ret_zero() {
        let http = HttpFixture::start(vec![batch(Vec::new(), "checked-cursor")]).await;
        let (_directory, state) = fixture(&http.base);
        let account = snapshot(&state).unwrap().unwrap();
        let check = state
            .db(|db| Ok(weixin_login::start_check(db, "admin", account.revision)))
            .unwrap()
            .unwrap();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_receiver_with(
            state.clone(),
            cancel.clone(),
            http.client(),
        ));
        until(|| {
            scalar(
                &state,
                "SELECT count(*) FROM weixin_checks WHERE status='passed'",
            ) == 1
        })
        .await;
        let latest = state
            .db(|db| weixin_login::latest_check(db, "admin"))
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, check.id);
        assert_eq!(latest.status, "passed");
        assert_eq!(snapshot(&state).unwrap().unwrap().cursor, "checked-cursor");
        assert_eq!(
            snapshot(&state).unwrap().unwrap().connection_status,
            "connected"
        );
        let session = "c".repeat(64);
        state.db(|db| {
            db.execute("INSERT INTO admins(id,username,password_hash,created_at) VALUES('status-admin','status-admin','unused-fixture',?1)",[now()])?;
            db.execute("INSERT INTO sessions(token_hash,user_id,csrf,expires_at) VALUES(?1,'status-admin','status-csrf',?2)",params![crate::state::token_hash(&session),now()+300])?;
            Ok(())
        }).unwrap();
        let response = crate::api::router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/admin/v1/weixin")
                    .header("Host", "127.0.0.1:18419")
                    .header("Cookie", format!("dsh_session={session}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(
            page["account"]["connection_status"], "connected",
            "the actual ret=0 result must reach the management/UI contract"
        );
        assert_eq!(scalar(&state, "SELECT count(*) FROM runs"), 0);
        stop(cancel, vec![task]).await;
        http.close().await;
    }

    #[tokio::test]
    async fn nul_text_is_quarantined_without_poisoning_other_messages_or_cursor_commit() {
        let http = HttpFixture::start(vec![batch(
            vec![
                message(41, "bad\u{0000}source"),
                message(42, "valid following text"),
            ],
            "after-nul-source",
        )])
        .await;
        let (_directory, state) = fixture(&http.base);
        let cancel = CancellationToken::new();
        let receiver = tokio::spawn(run_receiver_with(
            state.clone(),
            cancel.clone(),
            http.client(),
        ));
        until(|| snapshot(&state).unwrap().unwrap().cursor == "after-nul-source").await;
        assert_eq!(scalar(&state,"SELECT count(*) FROM weixin_inbox WHERE disposition='rejected' AND reason_code='malformed_event'"),1);
        assert_eq!(
            scalar(
                &state,
                "SELECT count(*) FROM runs WHERE input='valid following text'"
            ),
            1
        );
        assert_eq!(scalar(&state, "SELECT count(*) FROM runs"), 1);
        assert_eq!(
            scalar(
                &state,
                "SELECT count(*) FROM weixin_contexts WHERE token IS NOT NULL"
            ),
            1
        );
        assert_eq!(
            snapshot(&state).unwrap().unwrap().connection_status,
            "connected"
        );
        stop(cancel, vec![receiver]).await;
        http.close().await;
    }

    #[tokio::test]
    async fn nonzero_send_business_result_is_failed_and_expired_token_stops_both_loops() {
        let http = HttpFixture::start(vec![batch(
            vec![message(11, "will receive explicit send rejection")],
            "received",
        )])
        .await;
        *http.data.send_response.lock().unwrap() = json!({"ret":-14,"errcode":-14});
        let (_directory, state) = fixture(&http.base);
        let cancel = CancellationToken::new();
        let tasks = vec![
            tokio::spawn(run_receiver_with(
                state.clone(),
                cancel.clone(),
                http.client(),
            )),
            tokio::spawn(worker::run_worker(state.clone(), cancel.clone())),
            tokio::spawn(run_sender_with(
                state.clone(),
                cancel.clone(),
                http.client(),
            )),
        ];
        until(|| snapshot(&state).unwrap().unwrap().connection_status == "needs_login").await;
        assert_eq!(scalar(&state,"SELECT count(*) FROM weixin_outbox WHERE status='failed' AND error_code='business_-14_-14'"),1);
        assert_eq!(
            scalar(&state, "SELECT count(*) FROM runs WHERE status='completed'"),
            1
        );
        assert_eq!(http.data.sends.load(Ordering::SeqCst), 1);
        stop(cancel, tasks).await;
        http.close().await;
    }
}
