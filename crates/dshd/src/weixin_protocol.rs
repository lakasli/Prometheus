//! Bounded iLink text protocol; no account activation, persistence or retries.
//!
//! Independently implemented against Tencent/openclaw-weixin 2.4.10, commit
//! 874637a0dbc24050f72d1385add4fc6170a2d726 (src/api/{api,types}.ts,
//! src/auth/login-qr.ts). These describe client behavior, not a complete server
//! contract. DSH limits below are local resource limits, not platform quotas.
use crate::config::public_ip;
use reqwest::{header, Method};
use serde_json::{json, Value};
use std::{fmt, net::SocketAddr, time::Duration};
use tokio_util::sync::CancellationToken;
use url::Url;

pub const UPSTREAM_COMMIT: &str = "874637a0dbc24050f72d1385add4fc6170a2d726";
pub const CHANNEL_VERSION: &str = "2.4.10";
pub const CLIENT_VERSION: &str = "132106";
pub const DEFAULT_BASE: &str = "https://ilinkai.weixin.qq.com/";
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
pub const MAX_BATCH_MESSAGES: usize = 100;
pub const MAX_OPAQUE_BYTES: usize = 64 * 1024;
pub const MAX_INPUT_BYTES: usize = 16 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024;
pub const API_TIMEOUT: Duration = Duration::from_secs(15);
pub const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidInput(&'static str),
    InvalidResponse(&'static str),
    LimitExceeded(&'static str),
    UntrustedEndpoint,
    PrivateAddress,
    Dns,
    Http(u16),
    Business {
        ret: Option<i64>,
        errcode: Option<i64>,
    },
    Transport,
    Timeout,
    Cancelled,
}
impl ProtocolError {
    pub fn is_stale_token(&self) -> bool {
        matches!(
            self,
            Self::Business { ret: Some(-14), .. }
                | Self::Business {
                    errcode: Some(-14),
                    ..
                }
        )
    }
}
impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never attach upstream bodies, query strings, URLs or reqwest errors:
        // even a transport error can otherwise include a QR/verification code.
        write!(f, "iLink {self:?}")
    }
}
impl std::error::Error for ProtocolError {}
pub type Result<T> = std::result::Result<T, ProtocolError>;

macro_rules! secret {
    ($name:ident, $field:literal, $maximum:expr) => {
        #[derive(Clone, PartialEq, Eq)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                if value.trim().is_empty() || value.chars().any(char::is_control) {
                    return Err(ProtocolError::InvalidInput($field));
                }
                if value.len() > $maximum {
                    return Err(ProtocolError::LimitExceeded($field));
                }
                Ok(Self(value))
            }
            /// Explicit wire/persistence access. This type deliberately has no
            /// Serialize implementation; management APIs must choose safe DTOs.
            pub fn expose(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "([redacted])"))
            }
        }
    };
}
secret!(SecretToken, "bot_token", MAX_OPAQUE_BYTES);
secret!(ContextToken, "context_token", MAX_OPAQUE_BYTES);
secret!(QrCode, "qrcode", MAX_OPAQUE_BYTES);
secret!(QrDisplay, "qrcode_display", MAX_OPAQUE_BYTES);
secret!(Cursor, "cursor", MAX_OPAQUE_BYTES);

#[derive(Clone, PartialEq, Eq)]
pub struct VerificationCode(String);
impl VerificationCode {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > 16 || !value.bytes().all(|c| c.is_ascii_digit()) {
            return Err(ProtocolError::InvalidInput("verification_code"));
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for VerificationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerificationCode([redacted])")
    }
}

/// A validated origin, with no credentials, arbitrary path, query or fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiBase(Url);
impl ApiBase {
    pub fn trusted_https(value: &str) -> Result<Self> {
        if !value.is_ascii()
            || value
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
            || value.contains(['\\', '%'])
        {
            return Err(ProtocolError::UntrustedEndpoint);
        }
        let url = Url::parse(value).map_err(|_| ProtocolError::UntrustedEndpoint)?;
        let host = url.host_str().ok_or(ProtocolError::UntrustedEndpoint)?;
        let raw_authority = value
            .strip_prefix("https://")
            .ok_or(ProtocolError::UntrustedEndpoint)?;
        let raw_path = raw_authority.find('/').map(|i| &raw_authority[i..]);
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.port_or_known_default() != Some(443)
            || url.path() != "/"
            || raw_path.is_some_and(|path| path != "/")
            || raw_authority.contains('@')
            || !(host == "weixin.qq.com" || host.ends_with(".weixin.qq.com"))
        {
            return Err(ProtocolError::UntrustedEndpoint);
        }
        Ok(Self(url))
    }
    pub fn from_redirect_host(host: &str) -> Result<Self> {
        if host.is_empty()
            || !host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.')
        {
            return Err(ProtocolError::UntrustedEndpoint);
        }
        Self::trusted_https(&format!("https://{host}/"))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Clone, Debug)]
pub struct QrTicket {
    pub qrcode: QrCode,
    /// Encode this locally as a QR, rather than fetching an arbitrary image URL.
    pub display_content: QrDisplay,
}
#[derive(Clone, Debug)]
pub struct LoginCredentials {
    pub token: SecretToken,
    pub account_id: String,
    pub owner_id: String,
    pub base: ApiBase,
}
#[derive(Clone, Debug)]
pub enum QrStatus {
    Waiting,
    Scanned,
    Confirmed(LoginCredentials),
    Expired,
    NeedVerification,
    VerificationBlocked,
    Redirect(ApiBase),
    /// No credentials have been issued. The caller must verify an existing
    /// matching local account before treating this as an established session.
    AlreadyBound {
        account_id: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    Group,
    BotMessage,
    UnknownMessageType,
    NotFinished,
    Deleted,
    MediaOrUnknownItem,
    EmptyText,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuarantineReason {
    MalformedEvent,
    MissingMessageId,
    InvalidMessageId,
    MissingSender,
    MissingRecipient,
    MissingContext,
    InvalidContext,
    InputTooLarge,
}
#[derive(Clone, Debug)]
pub struct EventRejection<R> {
    pub message_id: Option<String>,
    pub reason: R,
}
#[derive(Clone)]
pub struct InboundText {
    pub message_id: String,
    pub from_user_id: String,
    pub to_user_id: String,
    pub created_at_ms: Option<u64>,
    pub text: String,
    pub context_token: ContextToken,
}
impl fmt::Debug for InboundText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InboundText([private content redacted])")
    }
}
#[derive(Clone, Debug)]
pub enum InboundDisposition {
    Text(InboundText),
    Unsupported(EventRejection<UnsupportedReason>),
    Quarantine(EventRejection<QuarantineReason>),
}
#[derive(Clone, Debug)]
pub struct Updates {
    pub events: Vec<InboundDisposition>,
    /// None means retain the previous cursor, including for a returned "".
    pub next_cursor: Option<Cursor>,
    pub next_timeout: Duration,
}
#[derive(Clone)]
pub struct TextReply {
    pub to_user_id: String,
    pub client_id: String,
    pub text: String,
    pub context_token: ContextToken,
    pub run_id: Option<String>,
}
impl fmt::Debug for TextReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TextReply([private content redacted])")
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendAccepted {
    /// Platform acceptance is not proof that the phone displayed the message.
    pub server_message_id: Option<String>,
}

#[derive(Clone, Default)]
pub struct WeixinClient {
    #[cfg(test)]
    loopback: Option<ApiBase>,
}
impl WeixinClient {
    pub fn production() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn for_test(base: &str, allow_loopback: bool) -> Result<(Self, ApiBase)> {
        let url = Url::parse(base).map_err(|_| ProtocolError::UntrustedEndpoint)?;
        let ip = url
            .host_str()
            .and_then(|s| s.parse::<std::net::IpAddr>().ok());
        if !allow_loopback
            || !ip.is_some_and(|ip| ip.is_loopback())
            || url.scheme() != "http"
            || url.path() != "/"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ProtocolError::UntrustedEndpoint);
        }
        let base = ApiBase(url);
        Ok((
            Self {
                loopback: Some(base.clone()),
            },
            base,
        ))
    }

    fn qr_base(&self) -> ApiBase {
        #[cfg(test)]
        if let Some(base) = &self.loopback {
            return base.clone();
        }
        ApiBase::trusted_https(DEFAULT_BASE).expect("fixed trusted iLink origin")
    }

    pub async fn fetch_qr(
        &self,
        local_tokens: &[&SecretToken],
        cancel: &CancellationToken,
    ) -> Result<QrTicket> {
        if local_tokens.len() > 10 {
            return Err(ProtocolError::LimitExceeded("local_token_list"));
        }
        let mut url = self.qr_base().0;
        url.set_path("/ilink/bot/get_bot_qrcode");
        url.query_pairs_mut().append_pair("bot_type", "3");
        let body =
            json!({"local_token_list":local_tokens.iter().map(|s|s.expose()).collect::<Vec<_>>()});
        let value = self
            .request(Method::POST, url, Some(body), None, API_TIMEOUT, cancel)
            .await?;
        check_business(&value, false)?;
        Ok(QrTicket {
            qrcode: QrCode::new(required_string(&value, "qrcode", MAX_OPAQUE_BYTES)?)
                .map_err(server_secret_error)?,
            display_content: QrDisplay::new(required_string(
                &value,
                "qrcode_img_content",
                MAX_OPAQUE_BYTES,
            )?)
            .map_err(server_secret_error)?,
        })
    }

    pub async fn poll_qr(
        &self,
        qrcode: &QrCode,
        verify: Option<&VerificationCode>,
        redirected_base: Option<&ApiBase>,
        cancel: &CancellationToken,
    ) -> Result<QrStatus> {
        let mut url = redirected_base.cloned().unwrap_or_else(|| self.qr_base()).0;
        url.set_path("/ilink/bot/get_qrcode_status");
        url.query_pairs_mut().append_pair("qrcode", qrcode.expose());
        if let Some(code) = verify {
            url.query_pairs_mut()
                .append_pair("verify_code", code.expose());
        }
        let value = self
            .request(Method::GET, url, None, None, LONG_POLL_TIMEOUT, cancel)
            .await?;
        decode_qr_status(&value)
    }

    pub async fn get_updates(
        &self,
        base: &ApiBase,
        token: &SecretToken,
        cursor: Option<&Cursor>,
        timeout: Option<Duration>,
        cancel: &CancellationToken,
    ) -> Result<Updates> {
        let body = json!({"get_updates_buf":cursor.map(Cursor::expose).unwrap_or(""),"base_info":base_info()});
        let mut url = base.0.clone();
        url.set_path("/ilink/bot/getupdates");
        let value = self
            .request(
                Method::POST,
                url,
                Some(body),
                Some(token),
                bounded_poll_timeout(timeout.unwrap_or(LONG_POLL_TIMEOUT)),
                cancel,
            )
            .await?;
        decode_updates(&value)
    }

    /// Never retries a send. A transport/timeout/cancellation after dispatch is
    /// ambiguous: the caller must keep the durable outbox entry for inspection.
    pub async fn send_text(
        &self,
        base: &ApiBase,
        token: &SecretToken,
        reply: &TextReply,
        cancel: &CancellationToken,
    ) -> Result<SendAccepted> {
        validate_identifier(&reply.to_user_id, "to_user_id")?;
        validate_identifier(&reply.client_id, "client_id")?;
        if let Some(id) = &reply.run_id {
            validate_identifier(id, "run_id")?;
        }
        if reply.text.trim().is_empty() {
            return Err(ProtocolError::InvalidInput("text"));
        }
        if reply.text.len() > MAX_OUTPUT_BYTES {
            return Err(ProtocolError::LimitExceeded("output_text"));
        }
        let mut msg = json!({"from_user_id":"","to_user_id":reply.to_user_id,"client_id":reply.client_id,"message_type":2,"message_state":2,"context_token":reply.context_token.expose(),"item_list":[{"type":1,"text_item":{"text":reply.text}}]});
        if let Some(run) = &reply.run_id {
            msg["run_id"] = Value::String(run.clone());
        }
        let mut url = base.0.clone();
        url.set_path("/ilink/bot/sendmessage");
        let value = self
            .request(
                Method::POST,
                url,
                Some(json!({"msg":msg,"base_info":base_info()})),
                Some(token),
                API_TIMEOUT,
                cancel,
            )
            .await?;
        check_business(&value, true)?;
        Ok(SendAccepted {
            server_message_id: optional_id(&value, "message_id")?,
        })
    }

    async fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<Value>,
        token: Option<&SecretToken>,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let encoded = body
            .map(|v| serde_json::to_vec(&v).map_err(|_| ProtocolError::InvalidInput("json")))
            .transpose()?;
        if encoded.as_ref().is_some_and(|v| v.len() > MAX_BODY_BYTES) {
            return Err(ProtocolError::LimitExceeded("request_body"));
        }
        let operation = async {
            let client = self.pinned_client(&url).await?;
            let is_post = method == Method::POST;
            let mut request = client
                .request(method, url)
                .header("iLink-App-Id", "bot")
                .header("iLink-App-ClientVersion", CLIENT_VERSION)
                .header(header::USER_AGENT, "DSH/0.1 iLink")
                .header(header::ACCEPT, "application/json");
            if is_post {
                request = request
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("AuthorizationType", "ilink_bot_token")
                    .header("X-WECHAT-UIN", wechat_uin(rand::random()));
                if let Some(token) = token {
                    if !token.expose().bytes().all(|c| c.is_ascii_graphic()) {
                        return Err(ProtocolError::InvalidInput("bot_token"));
                    }
                    let mut authorization =
                        header::HeaderValue::from_str(&format!("Bearer {}", token.expose()))
                            .map_err(|_| ProtocolError::InvalidInput("bot_token"))?;
                    authorization.set_sensitive(true);
                    request = request.header(header::AUTHORIZATION, authorization);
                }
            }
            if let Some(encoded) = encoded {
                request = request.body(encoded);
            }
            let mut response = request.send().await.map_err(|_| ProtocolError::Transport)?;
            if !response.status().is_success() {
                return Err(ProtocolError::Http(response.status().as_u16()));
            }
            if response
                .content_length()
                .is_some_and(|n| n > MAX_BODY_BYTES as u64)
            {
                return Err(ProtocolError::LimitExceeded("response_body"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| ProtocolError::Transport)?
            {
                if chunk.len() > MAX_BODY_BYTES.saturating_sub(bytes.len()) {
                    return Err(ProtocolError::LimitExceeded("response_body"));
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|_| ProtocolError::InvalidResponse("json"))
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ProtocolError::Cancelled),
            result = tokio::time::timeout(timeout, operation) => result.map_err(|_| ProtocolError::Timeout)?,
        }
    }

    async fn pinned_client(&self, url: &Url) -> Result<reqwest::Client> {
        let host = url.host_str().ok_or(ProtocolError::UntrustedEndpoint)?;
        let port = url
            .port_or_known_default()
            .ok_or(ProtocolError::UntrustedEndpoint)?;
        let mut origin = url.clone();
        origin.set_path("/");
        origin.set_query(None);
        origin.set_fragment(None);
        #[cfg(test)]
        let allow_loopback = self.loopback.as_ref().is_some_and(|b| b.0 == origin);
        #[cfg(not(test))]
        let allow_loopback = false;
        if !allow_loopback {
            ApiBase::trusted_https(origin.as_str())?;
        }
        let addresses: Vec<SocketAddr> = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| ProtocolError::Dns)?
        .map_err(|_| ProtocolError::Dns)?
        .collect();
        validate_addresses(&addresses, allow_loopback)?;
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|_| ProtocolError::Transport)
    }
}

fn validate_addresses(addresses: &[SocketAddr], allow_loopback: bool) -> Result<()> {
    if addresses.is_empty() {
        return Err(ProtocolError::Dns);
    }
    if addresses.iter().any(|a| {
        if allow_loopback {
            !a.ip().is_loopback()
        } else {
            !public_ip(a.ip())
        }
    }) {
        return Err(ProtocolError::PrivateAddress);
    }
    Ok(())
}
fn base_info() -> Value {
    json!({"channel_version":CHANNEL_VERSION,"bot_agent":"DSH/0.1"})
}
fn bounded_poll_timeout(timeout: Duration) -> Duration {
    timeout.clamp(Duration::from_secs(1), LONG_POLL_TIMEOUT)
}
fn validate_identifier(value: &str, name: &'static str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(ProtocolError::InvalidInput(name));
    }
    Ok(())
}
fn server_secret_error(error: ProtocolError) -> ProtocolError {
    match error {
        ProtocolError::InvalidInput(name) => ProtocolError::InvalidResponse(name),
        other => other,
    }
}
fn required_string(value: &Value, name: &'static str, maximum: usize) -> Result<String> {
    let text = value
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
        .ok_or(ProtocolError::InvalidResponse(name))?;
    if text.len() > maximum {
        return Err(ProtocolError::LimitExceeded(name));
    }
    Ok(text.into())
}
fn optional_code(value: &Value, name: &'static str) -> Result<Option<i64>> {
    value
        .get(name)
        .map(|v| v.as_i64().ok_or(ProtocolError::InvalidResponse(name)))
        .transpose()
}
fn check_business(value: &Value, require_ret: bool) -> Result<()> {
    if !value.is_object() {
        return Err(ProtocolError::InvalidResponse("object"));
    }
    let ret = optional_code(value, "ret")?;
    let errcode = optional_code(value, "errcode")?;
    if ret.is_some_and(|c| c != 0) || errcode.is_some_and(|c| c != 0) {
        return Err(ProtocolError::Business { ret, errcode });
    }
    if require_ret && ret.is_none() {
        return Err(ProtocolError::InvalidResponse("missing_ret"));
    }
    Ok(())
}
fn lossless_id(value: &Value) -> Result<String> {
    let number = match value {
        Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse::<u64>().ok()
        }
        Value::Number(n) => n.as_u64(),
        _ => None,
    };
    number
        .map(|v| v.to_string())
        .ok_or(ProtocolError::InvalidResponse("message_id"))
}
fn optional_id(value: &Value, name: &'static str) -> Result<Option<String>> {
    value
        .get(name)
        .filter(|v| !v.is_null())
        .map(lossless_id)
        .transpose()
}

pub fn decode_qr_status(value: &Value) -> Result<QrStatus> {
    check_business(value, false)?;
    match value.get("status").and_then(Value::as_str) {
        Some("wait") => Ok(QrStatus::Waiting),
        Some("scaned") => Ok(QrStatus::Scanned),
        Some("expired") => Ok(QrStatus::Expired),
        Some("need_verifycode") => Ok(QrStatus::NeedVerification),
        Some("verify_code_blocked") => Ok(QrStatus::VerificationBlocked),
        Some("scaned_but_redirect") => Ok(QrStatus::Redirect(ApiBase::from_redirect_host(
            &required_string(value, "redirect_host", 256)?,
        )?)),
        Some("binded_redirect") => Ok(QrStatus::AlreadyBound {
            account_id: value
                .get("ilink_bot_id")
                .and_then(Value::as_str)
                .filter(|s| validate_identifier(s, "account_id").is_ok())
                .map(str::to_owned),
        }),
        Some("confirmed") => {
            let account_id = required_string(value, "ilink_bot_id", 256)?;
            let owner_id = required_string(value, "ilink_user_id", 256)?;
            validate_identifier(&account_id, "account_id").map_err(server_secret_error)?;
            validate_identifier(&owner_id, "owner_id").map_err(server_secret_error)?;
            let token = SecretToken::new(required_string(value, "bot_token", MAX_OPAQUE_BYTES)?)
                .map_err(server_secret_error)?;
            if !token.expose().bytes().all(|c| c.is_ascii_graphic()) {
                return Err(ProtocolError::InvalidResponse("bot_token"));
            }
            Ok(QrStatus::Confirmed(LoginCredentials {
                token,
                account_id,
                owner_id,
                base: ApiBase::trusted_https(&required_string(value, "baseurl", 1024)?)?,
            }))
        }
        _ => Err(ProtocolError::InvalidResponse("qr_status")),
    }
}

pub fn decode_updates(value: &Value) -> Result<Updates> {
    check_business(value, true)?;
    let messages = match value.get("msgs") {
        None => &[][..],
        Some(Value::Array(messages)) => messages.as_slice(),
        _ => return Err(ProtocolError::InvalidResponse("msgs")),
    };
    if messages.len() > MAX_BATCH_MESSAGES {
        return Err(ProtocolError::LimitExceeded("batch"));
    }
    let next_cursor = match value.get("get_updates_buf") {
        None => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => Some(Cursor::new(s.clone()).map_err(server_secret_error)?),
        _ => return Err(ProtocolError::InvalidResponse("cursor")),
    };
    let next_timeout = match value.get("longpolling_timeout_ms") {
        None => LONG_POLL_TIMEOUT,
        Some(v) => bounded_poll_timeout(Duration::from_millis(
            v.as_u64()
                .ok_or(ProtocolError::InvalidResponse("longpolling_timeout_ms"))?,
        )),
    };
    Ok(Updates {
        events: messages.iter().map(decode_inbound).collect(),
        next_cursor,
        next_timeout,
    })
}

pub fn decode_inbound(value: &Value) -> InboundDisposition {
    use InboundDisposition::{Quarantine, Unsupported};
    let id = match optional_id(value, "message_id") {
        Ok(id) => id.or_else(|| {
            value
                .get("item_list")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find_map(|i| optional_id(i, "msg_id").ok().flatten())
                })
        }),
        Err(_) => {
            return Quarantine(EventRejection {
                message_id: None,
                reason: QuarantineReason::InvalidMessageId,
            })
        }
    };
    let quarantine = |reason| {
        Quarantine(EventRejection {
            message_id: id.clone(),
            reason,
        })
    };
    let unsupported = |reason| {
        Unsupported(EventRejection {
            message_id: id.clone(),
            reason,
        })
    };
    if !value.is_object() {
        return quarantine(QuarantineReason::MalformedEvent);
    }
    match value.get("group_id") {
        Some(Value::String(group)) if !group.is_empty() => {
            return unsupported(UnsupportedReason::Group)
        }
        None | Some(Value::Null) | Some(Value::String(_)) => (),
        _ => return quarantine(QuarantineReason::MalformedEvent),
    }
    if value
        .get("delete_time_ms")
        .and_then(Value::as_u64)
        .is_some_and(|v| v > 0)
    {
        return unsupported(UnsupportedReason::Deleted);
    }
    match value.get("message_type").and_then(Value::as_u64) {
        Some(1) => (),
        Some(2) => return unsupported(UnsupportedReason::BotMessage),
        Some(_) => return unsupported(UnsupportedReason::UnknownMessageType),
        None => return quarantine(QuarantineReason::MalformedEvent),
    }
    match value.get("message_state").and_then(Value::as_u64) {
        Some(2) => (),
        Some(_) => return unsupported(UnsupportedReason::NotFinished),
        None => return quarantine(QuarantineReason::MalformedEvent),
    }
    let Some(message_id) = id.clone() else {
        return quarantine(QuarantineReason::MissingMessageId);
    };
    let Ok(sender) = required_string(value, "from_user_id", 256) else {
        return quarantine(QuarantineReason::MissingSender);
    };
    let Ok(recipient) = required_string(value, "to_user_id", 256) else {
        return quarantine(QuarantineReason::MissingRecipient);
    };
    if validate_identifier(&sender, "sender").is_err() {
        return quarantine(QuarantineReason::MissingSender);
    }
    if validate_identifier(&recipient, "recipient").is_err() {
        return quarantine(QuarantineReason::MissingRecipient);
    }
    let context = match value.get("context_token") {
        Some(Value::String(s)) if !s.is_empty() => match ContextToken::new(s.clone()) {
            Ok(token) => token,
            Err(_) => return quarantine(QuarantineReason::InvalidContext),
        },
        _ => return quarantine(QuarantineReason::MissingContext),
    };
    let Some(items) = value.get("item_list").and_then(Value::as_array) else {
        return quarantine(QuarantineReason::MalformedEvent);
    };
    if items
        .iter()
        .any(|i| i.get("type").and_then(Value::as_u64) != Some(1))
    {
        return unsupported(UnsupportedReason::MediaOrUnknownItem);
    }
    let mut text = String::new();
    for item in items {
        let Some(part) = item
            .get("text_item")
            .and_then(|v| v.get("text"))
            .and_then(Value::as_str)
        else {
            return quarantine(QuarantineReason::MalformedEvent);
        };
        if part.contains('\0') {
            return quarantine(QuarantineReason::MalformedEvent);
        }
        let separator = usize::from(!text.is_empty());
        if text
            .len()
            .saturating_add(separator)
            .saturating_add(part.len())
            > MAX_INPUT_BYTES
        {
            return quarantine(QuarantineReason::InputTooLarge);
        }
        if separator != 0 {
            text.push('\n');
        }
        text.push_str(part);
    }
    if text.trim().is_empty() {
        return unsupported(UnsupportedReason::EmptyText);
    }
    InboundDisposition::Text(InboundText {
        message_id,
        from_user_id: sender,
        to_user_id: recipient,
        created_at_ms: value.get("create_time_ms").and_then(Value::as_u64),
        text,
        context_token: context,
    })
}

/// Only used for the decimal representation of a random uint32, not arbitrary
/// secret encoding. Avoids adding a dependency just for this tiny header.
fn wechat_uin(number: u32) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let decimal = number.to_string();
    let mut output = String::new();
    for chunk in decimal.as_bytes().chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        output.push(ALPHABET[(a >> 2) as usize] as char);
        output.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        extract::State,
        http::{HeaderMap, Request, Response, StatusCode},
        routing::any,
        Router,
    };
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Debug)]
    struct Captured {
        method: Method,
        path: String,
        headers: HeaderMap,
        body: Value,
    }
    struct MockResponse {
        status: StatusCode,
        body: String,
        chunks: Option<Vec<Vec<u8>>>,
        location: Option<String>,
        delay: Duration,
    }
    impl MockResponse {
        fn json(value: Value) -> Self {
            Self {
                status: StatusCode::OK,
                body: value.to_string(),
                chunks: None,
                location: None,
                delay: Duration::ZERO,
            }
        }
    }
    struct MockState {
        captured: Mutex<Vec<Captured>>,
        replies: Mutex<VecDeque<MockResponse>>,
        received: tokio::sync::Notify,
    }
    struct MockServer {
        base: ApiBase,
        client: WeixinClient,
        state: Arc<MockState>,
        handle: tokio::task::JoinHandle<()>,
    }
    impl Drop for MockServer {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }
    impl MockServer {
        async fn start(replies: Vec<MockResponse>) -> Self {
            async fn serve(
                State(state): State<Arc<MockState>>,
                request: Request<Body>,
            ) -> Response<Body> {
                let (parts, body) = request.into_parts();
                let bytes = to_bytes(body, MAX_BODY_BYTES + 1).await.unwrap();
                state.captured.lock().unwrap().push(Captured {
                    method: parts.method,
                    path: parts.uri.to_string(),
                    headers: parts.headers,
                    body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                });
                let reply = state
                    .replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| MockResponse::json(json!({"ret":0})));
                state.received.notify_one();
                tokio::time::sleep(reply.delay).await;
                let mut response = Response::builder()
                    .status(reply.status)
                    .header(header::CONTENT_TYPE, "application/json");
                if let Some(location) = reply.location {
                    response = response.header(header::LOCATION, location);
                }
                let body = if let Some(chunks) = reply.chunks {
                    Body::from_stream(futures_util::stream::iter(
                        chunks.into_iter().map(Ok::<_, std::io::Error>),
                    ))
                } else {
                    Body::from(reply.body)
                };
                response.body(body).unwrap()
            }
            let state = Arc::new(MockState {
                captured: Mutex::new(Vec::new()),
                replies: Mutex::new(replies.into()),
                received: tokio::sync::Notify::new(),
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (client, base) = WeixinClient::for_test(
                &format!("http://{}/", listener.local_addr().unwrap()),
                true,
            )
            .unwrap();
            let app = Router::new().fallback(any(serve)).with_state(state.clone());
            let handle = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                base,
                client,
                state,
                handle,
            }
        }
        fn captured(&self) -> Vec<Captured> {
            self.state.captured.lock().unwrap().clone()
        }
    }
    fn inbound() -> Value {
        json!({"message_id":"18446744073709551615","from_user_id":"peer@im.wechat","to_user_id":"account@im.bot","message_type":1,"message_state":2,"context_token":"private-context","item_list":[{"type":1,"text_item":{"text":"用户明确发送的文本"}}],"create_time_ms":1792830000000u64})
    }
    fn reply() -> TextReply {
        TextReply {
            to_user_id: "peer@im.wechat".into(),
            client_id: "durable-outbox-1".into(),
            text: "回复文本".into(),
            context_token: ContextToken::new("reply-context").unwrap(),
            run_id: Some("run-1".into()),
        }
    }

    #[test]
    fn trusted_origins_reject_credential_path_and_host_confusion() {
        for value in [
            DEFAULT_BASE,
            "https://ilinkai.weixin.qq.com",
            "https://ilinkai2.weixin.qq.com:443/",
        ] {
            assert!(ApiBase::trusted_https(value).is_ok(), "{value}");
        }
        for value in [
            "http://ilinkai.weixin.qq.com/",
            "https://user@ilinkai.weixin.qq.com/",
            "https://@ilinkai.weixin.qq.com/",
            "https://ilinkai.weixin.qq.com.evil.test/",
            "https://evilweixin.qq.com/",
            "https://ilinkai.weixin.qq.com:444/",
            "https://ilinkai.weixin.qq.com/path",
            "https://ilinkai.weixin.qq.com/..",
            "https://ilinkai.weixin.qq.com/%2e",
            "https://ilinkai.weixin.qq.com/?q=1",
            "https://ilinkai.weixin.qq.com/#fragment",
            "https://127.0.0.1/",
            "https://[::1]/",
            "https://ilin\nkai.weixin.qq.com/",
            "https://ilinkai.weixin.qq.com\\@example.com/",
        ] {
            assert_eq!(
                ApiBase::trusted_https(value),
                Err(ProtocolError::UntrustedEndpoint),
                "{value}"
            );
        }
        assert!(ApiBase::from_redirect_host("ilinkai2.weixin.qq.com").is_ok());
        for host in [
            "ilinkai2.weixin.qq.com:443",
            "https://ilinkai2.weixin.qq.com",
            "ilinkai.weixin.qq.com/path",
            "127.0.0.1",
        ] {
            assert!(ApiBase::from_redirect_host(host).is_err());
        }
        assert!(WeixinClient::for_test("http://127.0.0.1:1234/", false).is_err());
        assert!(WeixinClient::for_test("http://192.168.1.2/", true).is_err());
        assert!(WeixinClient::for_test("http://localhost/", true).is_err());
    }

    #[test]
    fn dns_must_be_entirely_public_and_loopback_exception_is_narrow() {
        let public = "8.8.8.8:443".parse().unwrap();
        let local = "127.0.0.1:1234".parse().unwrap();
        let private = "192.168.1.1:443".parse().unwrap();
        assert!(validate_addresses(&[public], false).is_ok());
        assert_eq!(
            validate_addresses(&[public, private], false),
            Err(ProtocolError::PrivateAddress)
        );
        assert_eq!(
            validate_addresses(&[local], false),
            Err(ProtocolError::PrivateAddress)
        );
        assert_eq!(validate_addresses(&[], false), Err(ProtocolError::Dns));
        assert!(validate_addresses(&[local], true).is_ok());
        assert_eq!(
            validate_addresses(&[public], true),
            Err(ProtocolError::PrivateAddress)
        );
    }

    #[test]
    fn qr_status_is_explicit_and_confirmation_requires_all_credentials() {
        for (status, expected) in [
            ("wait", "Waiting"),
            ("scaned", "Scanned"),
            ("expired", "Expired"),
            ("need_verifycode", "NeedVerification"),
            ("verify_code_blocked", "VerificationBlocked"),
        ] {
            assert_eq!(
                format!("{:?}", decode_qr_status(&json!({"status":status})).unwrap()),
                expected
            );
        }
        assert!(matches!(
            decode_qr_status(&json!({"status":"binded_redirect"})).unwrap(),
            QrStatus::AlreadyBound { account_id: None }
        ));
        assert!(matches!(
            decode_qr_status(
                &json!({"status":"scaned_but_redirect","redirect_host":"ilinkai2.weixin.qq.com"})
            )
            .unwrap(),
            QrStatus::Redirect(_)
        ));
        assert!(decode_qr_status(
            &json!({"status":"scaned_but_redirect","redirect_host":"localhost"})
        )
        .is_err());
        let confirmed = json!({"status":"confirmed","bot_token":"sensitive-bot-token","ilink_bot_id":"account@im.bot","ilink_user_id":"owner@im.wechat","baseurl":DEFAULT_BASE});
        for name in ["bot_token", "ilink_bot_id", "ilink_user_id", "baseurl"] {
            let mut broken = confirmed.clone();
            broken.as_object_mut().unwrap().remove(name);
            assert!(decode_qr_status(&broken).is_err(), "{name}");
        }
        let result = decode_qr_status(&confirmed).unwrap();
        assert!(!format!("{result:?}").contains("sensitive-bot-token"));
        assert!(!format!("{:?}", VerificationCode::new("123456").unwrap()).contains("123456"));
        assert!(VerificationCode::new("1&other=2").is_err());
        assert_eq!(
            decode_qr_status(&json!({"status":"unknown","errmsg":"private"})).unwrap_err(),
            ProtocolError::InvalidResponse("qr_status")
        );
    }

    #[test]
    fn finished_private_text_is_lossless_and_does_not_debug_expose_context() {
        let mut wire = inbound();
        wire["message_id"] = Value::Number(u64::MAX.into());
        let InboundDisposition::Text(text) = decode_inbound(&wire) else {
            panic!("expected accepted text");
        };
        assert_eq!(text.message_id, "18446744073709551615");
        assert_eq!(text.text, "用户明确发送的文本");
        assert_eq!(text.context_token.expose(), "private-context");
        assert!(!format!("{text:?}").contains("private-context"));
        assert!(!format!("{:?}", reply()).contains("reply-context"));
        let parsed: Value = serde_json::from_str(r#"{"message_id":18446744073709551615}"#).unwrap();
        assert_eq!(
            optional_id(&parsed, "message_id").unwrap().unwrap(),
            "18446744073709551615"
        );
        assert!(lossless_id(&json!(18446744073709551616.0)).is_err());
        assert!(lossless_id(&json!(-1)).is_err());
        assert!(lossless_id(&json!(1.5)).is_err());
    }

    #[test]
    fn unsafe_or_unsupported_events_never_become_model_text() {
        for (field, value, reason) in [
            ("group_id", json!("group-1"), UnsupportedReason::Group),
            ("message_type", json!(2), UnsupportedReason::BotMessage),
            ("message_state", json!(1), UnsupportedReason::NotFinished),
            (
                "item_list",
                json!([{"type":2,"image_item":{}}]),
                UnsupportedReason::MediaOrUnknownItem,
            ),
        ] {
            let mut event = inbound();
            event[field] = value;
            assert!(
                matches!(decode_inbound(&event),InboundDisposition::Unsupported(EventRejection{reason:r,..}) if r==reason)
            );
        }
        for (field, value, reason) in [
            (
                "message_id",
                json!("not-a-uint64"),
                QuarantineReason::InvalidMessageId,
            ),
            ("from_user_id", json!(""), QuarantineReason::MissingSender),
            ("to_user_id", json!(""), QuarantineReason::MissingRecipient),
            ("context_token", json!(""), QuarantineReason::MissingContext),
            (
                "context_token",
                json!("x".repeat(MAX_OPAQUE_BYTES + 1)),
                QuarantineReason::InvalidContext,
            ),
            (
                "item_list",
                json!([{"type":1,"text_item":{"text":"x".repeat(MAX_INPUT_BYTES+1)}}]),
                QuarantineReason::InputTooLarge,
            ),
            (
                "item_list",
                json!([{"type":1,"text_item":{"text":"invalid\u{0000}text"}}]),
                QuarantineReason::MalformedEvent,
            ),
        ] {
            let mut event = inbound();
            event[field] = value;
            assert!(
                matches!(decode_inbound(&event),InboundDisposition::Quarantine(EventRejection{reason:r,..}) if r==reason)
            );
        }
        let mut missing = inbound();
        missing.as_object_mut().unwrap().remove("message_id");
        assert!(matches!(
            decode_inbound(&missing),
            InboundDisposition::Quarantine(EventRejection {
                reason: QuarantineReason::MissingMessageId,
                ..
            })
        ));
        missing["item_list"][0]["msg_id"] = json!("9007199254740993");
        assert!(
            matches!(decode_inbound(&missing),InboundDisposition::Text(InboundText{message_id,..}) if message_id=="9007199254740993")
        );
        let mut joined = inbound();
        joined["item_list"] = json!([
            {"type":1,"text_item":{"text":"x".repeat(MAX_INPUT_BYTES)}},
            {"type":1,"text_item":{"text":""}}
        ]);
        assert!(matches!(
            decode_inbound(&joined),
            InboundDisposition::Quarantine(EventRejection {
                reason: QuarantineReason::InputTooLarge,
                ..
            })
        ));
    }

    #[test]
    fn batch_limits_cursor_and_business_errors_are_not_silently_accepted() {
        let decoded =
            decode_updates(&json!({"ret":0,"get_updates_buf":"","longpolling_timeout_ms":999999}))
                .unwrap();
        assert!(decoded.next_cursor.is_none());
        assert_eq!(decoded.next_timeout, LONG_POLL_TIMEOUT);
        assert!(decoded.events.is_empty());
        assert_eq!(
            decode_updates(&json!({"ret":0,"msgs":vec![inbound();101]})).unwrap_err(),
            ProtocolError::LimitExceeded("batch")
        );
        assert_eq!(
            decode_updates(&json!({"ret":0,"get_updates_buf":"x".repeat(MAX_OPAQUE_BYTES+1)}))
                .unwrap_err(),
            ProtocolError::LimitExceeded("cursor")
        );
        assert_eq!(
            check_business(&json!({}), true),
            Err(ProtocolError::InvalidResponse("missing_ret"))
        );
        for wire in [
            json!({"ret":0,"errcode":-14,"errmsg":"private-context"}),
            json!({"ret":-14}),
            json!({"errcode":-14}),
        ] {
            let error = check_business(&wire, true).unwrap_err();
            assert!(error.is_stale_token());
            assert!(!format!("{error:?}").contains("private-context"));
        }
        assert_eq!(
            check_business(&json!({"ret":-2,"errcode":7}), true),
            Err(ProtocolError::Business {
                ret: Some(-2),
                errcode: Some(7)
            })
        );
        assert!(!ProtocolError::Http(401).is_stale_token());
        assert_eq!(wechat_uin(0), "MA==");
        assert_eq!(wechat_uin(u32::MAX), "NDI5NDk2NzI5NQ==");
    }

    #[tokio::test]
    async fn real_http_qr_headers_separate_login_poll_from_bot_authorization() {
        let server=MockServer::start(vec![MockResponse::json(json!({"qrcode":"private-qr","qrcode_img_content":"https://weixin.qq.com/login?secret=display"})),MockResponse::json(json!({"status":"need_verifycode"}))]).await;
        let old = SecretToken::new("old-token").unwrap();
        let cancel = CancellationToken::new();
        let ticket = server.client.fetch_qr(&[&old], &cancel).await.unwrap();
        assert!(!format!("{ticket:?}").contains("private-qr"));
        let code = VerificationCode::new("123456").unwrap();
        assert!(matches!(
            server
                .client
                .poll_qr(&ticket.qrcode, Some(&code), None, &cancel)
                .await
                .unwrap(),
            QrStatus::NeedVerification
        ));
        let captured = server.captured();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].method, Method::POST);
        assert_eq!(captured[0].path, "/ilink/bot/get_bot_qrcode?bot_type=3");
        assert_eq!(captured[0].body, json!({"local_token_list":["old-token"]}));
        assert_eq!(captured[0].headers["AuthorizationType"], "ilink_bot_token");
        assert!(captured[0].headers.contains_key("X-WECHAT-UIN"));
        assert!(!captured[0].headers.contains_key(header::AUTHORIZATION));
        assert_eq!(captured[1].method, Method::GET);
        assert_eq!(captured[1].headers["iLink-App-Id"], "bot");
        assert_eq!(
            captured[1].headers["iLink-App-ClientVersion"],
            CLIENT_VERSION
        );
        for key in ["AuthorizationType", "X-WECHAT-UIN", "Authorization"] {
            assert!(!captured[1].headers.contains_key(key));
        }
        assert_eq!(
            captured[1].path,
            "/ilink/bot/get_qrcode_status?qrcode=private-qr&verify_code=123456"
        );
    }

    #[tokio::test]
    async fn real_http_bot_requests_echo_cursor_and_frozen_reply_context() {
        let server = MockServer::start(vec![
            MockResponse::json(
                json!({"ret":0,"msgs":[inbound()],"get_updates_buf":"next-private-cursor"}),
            ),
            MockResponse::json(json!({"ret":0,"message_id":u64::MAX})),
        ])
        .await;
        let token = SecretToken::new("bot-secret").unwrap();
        let cursor = Cursor::new("previous-cursor").unwrap();
        let cancel = CancellationToken::new();
        let updates = server
            .client
            .get_updates(&server.base, &token, Some(&cursor), None, &cancel)
            .await
            .unwrap();
        assert_eq!(updates.next_cursor.unwrap().expose(), "next-private-cursor");
        let sent = server
            .client
            .send_text(&server.base, &token, &reply(), &cancel)
            .await
            .unwrap();
        assert_eq!(
            sent.server_message_id.as_deref(),
            Some("18446744073709551615")
        );
        let captured = server.captured();
        assert_eq!(captured.len(), 2);
        for request in &captured {
            assert_eq!(request.headers[header::AUTHORIZATION], "Bearer bot-secret");
            assert_eq!(request.headers["iLink-App-ClientVersion"], "132106");
            assert_eq!(request.body["base_info"]["channel_version"], "2.4.10");
            assert_eq!(request.body["base_info"]["bot_agent"], "DSH/0.1");
        }
        assert_eq!(captured[0].body["get_updates_buf"], "previous-cursor");
        let msg = &captured[1].body["msg"];
        assert_eq!(msg["context_token"], "reply-context");
        assert_eq!(msg["client_id"], "durable-outbox-1");
        assert_eq!(msg["message_type"], 2);
        assert_eq!(msg["message_state"], 2);
        assert_eq!(msg["item_list"][0]["text_item"]["text"], "回复文本");
        assert_eq!(msg["run_id"], "run-1");
    }

    #[tokio::test]
    async fn redirects_http_errors_and_chunked_overflow_fail_without_leaking_bodies() {
        let mut redirect = MockResponse::json(json!({"ret":0}));
        redirect.status = StatusCode::FOUND;
        redirect.location = Some("http://127.0.0.1:1/must-not-follow".into());
        let mut rejected = MockResponse::json(json!({"errmsg":"private-qr-and-token"}));
        rejected.status = StatusCode::UNAUTHORIZED;
        let mut oversized = MockResponse::json(Value::Null);
        oversized.chunks = Some(vec![vec![b'x'; MAX_BODY_BYTES], vec![b'x'; 1]]);
        let server = MockServer::start(vec![redirect, rejected, oversized]).await;
        let cancel = CancellationToken::new();
        assert_eq!(
            server.client.fetch_qr(&[], &cancel).await.unwrap_err(),
            ProtocolError::Http(302)
        );
        let error = server.client.fetch_qr(&[], &cancel).await.unwrap_err();
        assert_eq!(error, ProtocolError::Http(401));
        assert!(!format!("{error}").contains("private-qr"));
        assert_eq!(
            server.client.fetch_qr(&[], &cancel).await.unwrap_err(),
            ProtocolError::LimitExceeded("response_body")
        );
        assert_eq!(server.captured().len(), 3);
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_real_pending_long_poll_and_limits_prevent_dispatch() {
        let mut pending = MockResponse::json(json!({"ret":0}));
        pending.delay = Duration::from_secs(10);
        let server = MockServer::start(vec![pending]).await;
        let token = SecretToken::new("bot-secret").unwrap();
        let cancel = CancellationToken::new();
        let request = server
            .client
            .get_updates(&server.base, &token, None, None, &cancel);
        tokio::pin!(request);
        tokio::select! { _=server.state.received.notified()=>(), result=&mut request=>panic!("request completed before cancellation: {result:?}") }
        cancel.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), request)
                .await
                .unwrap()
                .unwrap_err(),
            ProtocolError::Cancelled
        );
        let fresh = CancellationToken::new();
        let mut oversized = reply();
        oversized.text = "x".repeat(MAX_OUTPUT_BYTES + 1);
        assert_eq!(
            server
                .client
                .send_text(&server.base, &token, &oversized, &fresh)
                .await
                .unwrap_err(),
            ProtocolError::LimitExceeded("output_text")
        );
        let tokens = vec![&token; 11];
        assert_eq!(
            server.client.fetch_qr(&tokens, &fresh).await.unwrap_err(),
            ProtocolError::LimitExceeded("local_token_list")
        );
        let escaped = SecretToken::new("\\".repeat(MAX_OPAQUE_BYTES)).unwrap();
        let tokens = vec![&escaped; 10];
        assert_eq!(
            server.client.fetch_qr(&tokens, &fresh).await.unwrap_err(),
            ProtocolError::LimitExceeded("request_body")
        );
        assert_eq!(server.captured().len(), 1);
    }
}
