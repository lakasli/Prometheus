//! QQ C2C media transport. Files stay private and are uploaded as bytes; there is
//! no model-controlled URL fetch, file path, or access-token-bearing redirect.
use crate::{
    config::{public_ip, RuntimeConfig},
    qq::access_token,
    state::{now, AppState},
    worker::ReplyTarget,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use md5::{Digest, Md5};
use serde_json::{json, Value};
use sha1::Sha1;
use std::{collections::BTreeMap, sync::OnceLock, time::Duration};

pub const QQ_API_BASE: &str = "https://api.bot.qq.com";
pub const DIRECT_UPLOAD_LIMIT: usize = 5 * 1024 * 1024;
pub const MEDIA_LIMIT: usize = 20 * 1024 * 1024;
const RESPONSE_LIMIT: usize = 512 * 1024;
static MEDIA_SLOT: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
pub type DeliveryResult<T> = Result<T, (bool, String)>;

#[derive(Debug)]
pub struct PlatformReply {
    pub http_status: u16,
    pub business_code: Option<i64>,
    pub trace_id: Option<String>,
    pub value: Value,
}
impl PlatformReply {
    pub fn accepted(&self) -> bool {
        matches!(self.http_status, 200 | 204) && self.business_code.unwrap_or(0) == 0
    }
    pub fn asynchronous(&self) -> bool {
        matches!(self.http_status, 201 | 202) || matches!(self.business_code, Some(304023 | 304024))
    }
    pub fn uncertain(&self) -> bool {
        self.asynchronous() || self.http_status == 408 || self.http_status >= 500
    }
    pub fn status_text(&self, stage: &str) -> String {
        format!(
            "QQ{stage}返回HTTP {}，业务码{}",
            self.http_status,
            self.business_code
                .map(|v| v.to_string())
                .unwrap_or_else(|| "未提供".into())
        )
    }
}

pub fn client() -> DeliveryResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("DSH/0.1 QQ-API")
        .build()
        .map_err(|_| (false, "QQ客户端初始化失败".into()))
}

/// Callers must supply a route selected from the server's catalog. Neither base
/// URL, headers nor HTTP method are supplied by a model or forwarded unchecked.
pub async fn request(
    state: &AppState,
    config: &RuntimeConfig,
    method: reqwest::Method,
    path: &str,
    query: &BTreeMap<String, String>,
    body: Option<&Value>,
) -> DeliveryResult<PlatformReply> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains(['?', '#', '\\'])
        || path.split('/').any(|part| matches!(part,"."|"..") || part.eq_ignore_ascii_case("%2e") || part.eq_ignore_ascii_case("%2e%2e") || part.eq_ignore_ascii_case(".%2e") || part.eq_ignore_ascii_case("%2e."))
    {
        return Err((false, "QQ接口路径无效".into()));
    }
    let token = access_token(state, config).await.map_err(|e| (false, e))?;
    request_http(
        QQ_API_BASE,
        &config.qq.app_id,
        &token,
        method,
        path,
        query,
        body,
    )
    .await
}

async fn request_http(
    base: &str,
    app_id: &str,
    token: &str,
    method: reqwest::Method,
    path: &str,
    query: &BTreeMap<String, String>,
    body: Option<&Value>,
) -> DeliveryResult<PlatformReply> {
    let mut req = client()?
        .request(method, format!("{base}{path}"))
        .header("Authorization", format!("QQBot {token}"))
        .header("X-Union-Appid", app_id)
        .query(query);
    if let Some(body) = body {
        req = req.json(body);
    }
    let mut response = req
        .send()
        .await
        .map_err(|_| (true, "QQ网络中断，操作结果未知，未自动重试".into()))?;
    let http_status = response.status().as_u16();
    let trace_header = response
        .headers()
        .get("x-tps-trace-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| s.len() <= 200)
        .map(str::to_string);
    if response.content_length().unwrap_or(0) > RESPONSE_LIMIT as u64 {
        return Err((true, "QQ响应超出上限，操作结果待核对".into()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| (true, "QQ响应接收中断，操作结果未知".into()))?
    {
        if bytes.len() + chunk.len() > RESPONSE_LIMIT {
            return Err((true, "QQ响应超出上限，操作结果待核对".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| (true, "QQ响应不是有效JSON，操作结果待核对".into()))?
    };
    let business_code = ["err_code", "code", "retcode"].iter().find_map(|key| {
        value[*key]
            .as_i64()
            .or_else(|| value[*key].as_str().and_then(|s| s.parse().ok()))
    });
    let trace_id = value["trace_id"]
        .as_str()
        .filter(|s| s.len() <= 200)
        .map(str::to_string)
        .or(trace_header);
    Ok(PlatformReply {
        http_status,
        business_code,
        trace_id,
        value,
    })
}

pub fn validate_target(
    state: &AppState,
    target: &ReplyTarget,
    seq: u32,
) -> DeliveryResult<RuntimeConfig> {
    if target.deadline < now() {
        return Err((false, "回复截止时间已过".into()));
    }
    if !matches!(target.kind.as_str(), "c2c" | "group" | "channel" | "dms")
        || target.target.is_empty()
        || target.target.len() > 200
        || !target
            .target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || seq == 0
        || (target.kind == "c2c" && seq > 4)
        || (target.kind == "group" && seq > 5)
    {
        return Err((false, "QQ目标或回复序号无效".into()));
    }
    let (_, config) = state
        .current_config()
        .map_err(|_| (false, "无法核验QQ当前配置".into()))?;
    if !config.qq.enabled
        || config.qq.app_id != target.account_id
        || config.qq.credential_ref.as_deref() != Some(target.credential_ref.as_str())
    {
        return Err((false, "QQ账号配置已变更，拒绝替换目标账号或凭证发送".into()));
    }
    Ok(config)
}

pub async fn send_message(
    state: &AppState,
    config: &RuntimeConfig,
    target: &ReplyTarget,
    body: &Value,
) -> DeliveryResult<String> {
    if target.deadline < now() {
        return Err((false, "回复截止时间已过".into()));
    }
    let path = match target.kind.as_str() {
        "c2c" => format!("/v2/users/{}/messages", target.target),
        "group" => format!("/v2/groups/{}/messages", target.target),
        "channel" => format!("/channels/{}/messages", target.target),
        "dms" => format!("/dms/{}/messages", target.target),
        _ => return Err((false, "未知QQ目标类型".into())),
    };
    let reply = request(
        state,
        config,
        reqwest::Method::POST,
        &path,
        &BTreeMap::new(),
        Some(body),
    )
    .await?;
    if !reply.accepted() {
        return Err((reply.uncertain(), reply.status_text("发送")));
    }
    reply.value["id"]
        .as_str()
        .or_else(|| reply.value["message"]["id"].as_str())
        .filter(|s| !s.is_empty() && s.len() <= 512)
        .map(str::to_string)
        .ok_or((true, "QQ响应无消息ID，结果未知".into()))
}

/// Verify MIME by bytes instead of a model's label. The size cap keeps one media
/// upload below the Pi's memory budget; uploads are serialized across sessions.
pub fn file_metadata(kind: &str, bytes: &[u8]) -> DeliveryResult<(u32, &'static str)> {
    if bytes.is_empty() || bytes.len() > MEDIA_LIMIT {
        return Err((false, "QQ媒体须为1字节到20MiB".into()));
    }
    match kind {
        "image" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Ok((1, "dsh-image.png")),
        "image" if bytes.starts_with(b"\xff\xd8\xff") => Ok((1, "dsh-image.jpg")),
        "video" if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" => Ok((2, "dsh-video.mp4")),
        "voice" if bytes.len() >= 44 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE" => {
            Ok((3, "dsh-voice.wav"))
        }
        "voice" if bytes.starts_with(b"#!SILK_V3") || bytes.starts_with(b"\x02#!SILK_V3") => {
            Ok((3, "dsh-voice.silk"))
        }
        "voice" if bytes.starts_with(b"OggS") => Ok((3, "dsh-voice.ogg")),
        "voice"
            if bytes.starts_with(b"ID3")
                || (bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0) =>
        {
            Ok((3, "dsh-voice.mp3"))
        }
        _ => Err((
            false,
            "QQ媒体格式不匹配；支持PNG/JPEG、MP4及WAV/MP3/OGG/SILK".into(),
        )),
    }
}

pub async fn send_media_bytes(
    state: &AppState,
    target: &ReplyTarget,
    kind: &str,
    bytes: &[u8],
    caption: &str,
    seq: u32,
) -> DeliveryResult<String> {
    let config = validate_target(state, target, seq)?;
    if target.kind != "c2c" {
        return Err((
            false,
            "此次富媒体发送仅启用QQ单聊，其他场景保持文字降级".into(),
        ));
    }
    let (file_type, file_name) = file_metadata(kind, bytes)?;
    let _slot = MEDIA_SLOT
        .get_or_init(|| tokio::sync::Semaphore::new(1))
        .try_acquire()
        .map_err(|_| (false, "QQ媒体上传繁忙，请稍后重试或使用文字".into()))?;
    let file_info = upload_bytes(state, &config, &target.target, file_type, file_name, bytes)
        .await
        .map_err(|(_, e)| (false, e))?;
    // Re-read account and deadline after upload: a revoked credential or switched
    // account must not send a prepared attachment into the previous context.
    let latest = validate_target(state, target, seq)?;
    let caption: String = if kind == "image" {
        caption.chars().take(1500).collect()
    } else {
        String::new()
    };
    let body = json!({"msg_type":7,"content":if caption.is_empty(){" "}else{&caption},"media":{"file_info":file_info},"msg_id":target.message_id,"msg_seq":seq});
    send_message(state, &latest, target, &body).await
}

pub async fn upload_bytes(
    state: &AppState,
    config: &RuntimeConfig,
    user: &str,
    file_type: u32,
    file_name: &str,
    bytes: &[u8],
) -> DeliveryResult<String> {
    let path = format!("/v2/users/{user}/files");
    let body = if bytes.len() <= DIRECT_UPLOAD_LIMIT {
        json!({"file_type":file_type,"file_name":file_name,"file_data":STANDARD.encode(bytes),"srv_send_msg":false})
    } else {
        let md5 = hex::encode(Md5::digest(bytes));
        let sha1 = hex::encode(Sha1::digest(bytes));
        let md5_10m = hex::encode(Md5::digest(&bytes[..bytes.len().min(10_002_432)]));
        let prepare = json!({"file_type":file_type,"file_size":bytes.len().to_string(),"file_name":file_name,"md5":md5,"sha1":sha1,"md5_10m":md5_10m});
        let reply = request(
            state,
            config,
            reqwest::Method::POST,
            &format!("/v2/users/{user}/upload_prepare"),
            &BTreeMap::new(),
            Some(&prepare),
        )
        .await?;
        if !reply.accepted() {
            return Err((false, reply.status_text("预上传")));
        }
        let upload_id = reply.value["upload_id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 512)
            .ok_or((false, "QQ预上传缺少upload_id".into()))?;
        let block = number(&reply.value["block_size"])
            .filter(|v| (1..=MEDIA_LIMIT as u64).contains(v))
            .ok_or((false, "QQ预上传分片大小无效".into()))? as usize;
        let parts = reply.value["parts"]
            .as_array()
            .filter(|p| !p.is_empty() && p.len() <= 128)
            .ok_or((false, "QQ预上传分片列表无效".into()))?;
        let expected = bytes.len().div_ceil(block);
        if parts.len() != expected {
            return Err((false, "QQ预上传分片数量不匹配".into()));
        }
        for (index, part) in parts.iter().enumerate() {
            if part["index"].as_u64() != Some(index as u64) {
                return Err((false, "QQ预上传分片序号无效".into()));
            }
            let begin = index * block;
            let chunk = &bytes[begin..(begin + block).min(bytes.len())];
            let signed = part["presigned_url"]
                .as_str()
                .ok_or((false, "QQ预上传缺少分片地址".into()))?;
            put_presigned(signed, chunk, None).await?;
            let finish = json!({"upload_id":upload_id,"part_index":index,"block_size":chunk.len().to_string(),"md5":hex::encode(Md5::digest(chunk))});
            let reply = request(
                state,
                config,
                reqwest::Method::POST,
                &format!("/v2/users/{user}/upload_part_finish"),
                &BTreeMap::new(),
                Some(&finish),
            )
            .await?;
            if !reply.accepted() {
                return Err((false, reply.status_text("分片确认")));
            }
        }
        json!({"file_type":file_type,"file_name":file_name,"upload_id":upload_id,"srv_send_msg":false})
    };
    let reply = request(
        state,
        config,
        reqwest::Method::POST,
        &path,
        &BTreeMap::new(),
        Some(&body),
    )
    .await?;
    if !reply.accepted() {
        // Uploads never send a message (srv_send_msg=false); an unknown upload is
        // safe to classify as a media failure, not an unknown user delivery.
        return Err((false, reply.status_text("媒体上传")));
    }
    reply.value["file_info"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 32768)
        .map(str::to_string)
        .ok_or((false, "QQ媒体上传缺少file_info".into()))
}

fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

/// Only short-lived URLs returned by the authenticated platform may reach this
/// helper. COS hosts are allowlisted, DNS answers are public and pinned, no QQ
/// Authorization header is attached and redirects are rejected.
pub async fn put_presigned(signed: &str, bytes: &[u8], mime: Option<&str>) -> DeliveryResult<()> {
    let url = url::Url::parse(signed).map_err(|_| (false, "QQ预签名上传地址无效".into()))?;
    let host = url.host_str().unwrap_or("").to_string();
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !(host.ends_with(".myqcloud.com")
            || host.ends_with(".qcloud.com")
            || host.ends_with(".qq.com"))
    {
        return Err((false, "QQ预签名上传目标不被允许".into()));
    }
    let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), 443))
        .await
        .map_err(|_| (false, "QQ上传地址解析失败".into()))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
        return Err((false, "QQ上传地址解析到非公网目标".into()));
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(10))
        .resolve_to_addrs(&host, &addresses)
        .build()
        .map_err(|_| (false, "QQ上传客户端初始化失败".into()))?;
    let mut req = client.put(url).body(bytes.to_vec());
    if let Some(mime) = mime {
        req = req.header(reqwest::header::CONTENT_TYPE, mime);
    }
    let reply = req
        .send()
        .await
        .map_err(|_| (false, "QQ分片上传网络失败".into()))?;
    if !reply.status().is_success() {
        return Err((
            false,
            format!("QQ分片上传返回HTTP {}", reply.status().as_u16()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Bytes,
        extract::State,
        http::{HeaderMap, Method, Uri},
        response::IntoResponse,
        Router,
    };
    use std::sync::{Arc, Mutex};
    #[test]
    fn media_signatures_and_caps_are_enforced() {
        assert_eq!(
            file_metadata("image", b"\x89PNG\r\n\x1a\nrest").unwrap().0,
            1
        );
        assert!(file_metadata("voice", b"<html>not audio").is_err());
        assert!(file_metadata("video", b"\x89PNG\r\n\x1a\n").is_err());
        assert!(file_metadata("image", &vec![0; MEDIA_LIMIT + 1]).is_err());
        let mut wav = vec![0; 44];
        wav[..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"WAVE");
        assert_eq!(file_metadata("voice", &wav).unwrap().1, "dsh-voice.wav");
    }
    #[test]
    fn asynchronous_platform_acceptance_is_not_delivered() {
        let r = PlatformReply {
            http_status: 202,
            business_code: Some(304024),
            trace_id: None,
            value: json!({}),
        };
        assert!(!r.accepted());
        assert!(r.asynchronous());
        let r = PlatformReply {
            http_status: 200,
            business_code: Some(40054013),
            trace_id: None,
            value: json!({}),
        };
        assert!(!r.accepted());
        assert!(!r.asynchronous());
        let r = PlatformReply {
            http_status: 500,
            business_code: None,
            trace_id: None,
            value: json!({}),
        };
        assert!(!r.accepted());
        assert!(r.uncertain());
        let r = PlatformReply {
            http_status: 504,
            business_code: Some(50055002),
            trace_id: None,
            value: json!({}),
        };
        assert!(!r.accepted());
        assert!(r.uncertain());
    }

    #[derive(Clone, Debug)]
    struct Receipt {
        method: String,
        path: String,
        body: Option<Value>,
        authorization: String,
        appid: String,
    }
    async fn mock_platform(
        State(receipts): State<Arc<Mutex<Vec<Receipt>>>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        bytes: Bytes,
    ) -> axum::response::Response {
        receipts.lock().unwrap().push(Receipt {
            method: method.to_string(),
            path: uri.to_string(),
            body: if bytes.is_empty() {
                None
            } else {
                Some(serde_json::from_slice(&bytes).unwrap())
            },
            authorization: headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string(),
            appid: headers
                .get("x-union-appid")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string(),
        });
        match uri.path() {
            "/__async" => (
                axum::http::StatusCode::ACCEPTED,
                axum::Json(json!({"code":304024})),
            )
                .into_response(),
            "/__business" => axum::Json(json!({"err_code":40054013})).into_response(),
            "/__empty" => axum::http::StatusCode::NO_CONTENT.into_response(),
            "/__invalid" => (axum::http::StatusCode::OK, "not-json").into_response(),
            "/__redirect" => (
                axum::http::StatusCode::TEMPORARY_REDIRECT,
                [("location", "https://example.invalid/never-follow")],
                "{}",
            )
                .into_response(),
            _ => axum::Json(
                json!({"id":"synthetic-platform-id","timestamp":"2026-10-25T20:00:00+08:00"}),
            )
            .into_response(),
        }
    }
    #[tokio::test]
    async fn every_catalog_operation_crosses_the_http_boundary_with_its_fixed_contract() {
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .fallback(mock_platform)
            .with_state(receipts.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut expected = Vec::new();
        for op in crate::qq_api::operations() {
            let parameters = op
                .parameters
                .iter()
                .map(|key| (key.clone(), format!("Test_{key}")))
                .collect();
            let body = if op.read_only || op.body_fields.is_empty() {
                None
            } else {
                Some(Value::Object(
                    op.required_body_fields
                        .iter()
                        .map(|key| (key.clone(), json!("synthetic")))
                        .collect(),
                ))
            };
            let input = crate::qq_api::ExecuteBody {
                parameters,
                query: op.query_defaults.clone(),
                body: body.clone(),
                ..Default::default()
            };
            let (path, query) = crate::qq_api::build_request(op, &input).unwrap();
            assert!(!path.contains(['{', '}']));
            let reply = request_http(
                &base,
                "synthetic-app",
                "synthetic-test-token",
                reqwest::Method::from_bytes(op.method.as_bytes()).unwrap(),
                &path,
                &query,
                body.as_ref(),
            )
            .await
            .unwrap();
            assert!(reply.accepted(), "{}", op.id);
            let encoded = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(query.iter())
                .finish();
            let expected_path = if encoded.is_empty() {
                path
            } else {
                format!("{path}?{encoded}")
            };
            expected.push((op.method.clone(), expected_path, body));
        }
        let recorded = receipts.lock().unwrap().clone();
        assert_eq!(
            recorded.len(),
            77,
            "catalog coverage changes require auditing the corresponding platform contracts"
        );
        for (receipt, (method, path, body)) in recorded.iter().zip(expected) {
            assert_eq!(receipt.method, method);
            assert_eq!(receipt.path, path);
            assert_eq!(receipt.body, body);
            assert_eq!(receipt.authorization, "QQBot synthetic-test-token");
            assert_eq!(receipt.appid, "synthetic-app");
        }
        for (path, accepted, asynchronous) in [
            ("/__async", false, true),
            ("/__business", false, false),
            ("/__empty", true, false),
            ("/__redirect", false, false),
        ] {
            let reply = request_http(
                &base,
                "synthetic-app",
                "synthetic-test-token",
                reqwest::Method::GET,
                path,
                &BTreeMap::new(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(reply.accepted(), accepted);
            assert_eq!(reply.asynchronous(), asynchronous);
        }
        let invalid = request_http(
            &base,
            "synthetic-app",
            "synthetic-test-token",
            reqwest::Method::POST,
            "/__invalid",
            &BTreeMap::new(),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            invalid.0,
            "invalid successful POST response must leave delivery unknown"
        );
        assert_eq!(
            receipts.lock().unwrap().len(),
            82,
            "unsafe requests must not redirect or retry"
        );
        server.abort();
    }

    #[tokio::test]
    async fn opaque_message_id_is_encoded_as_one_http_path_segment(){
        let receipts=Arc::new(Mutex::new(Vec::new()));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base=format!("http://{}",listener.local_addr().unwrap());
        let app=Router::new().fallback(mock_platform).with_state(receipts.clone());
        let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap()});
        let op=crate::qq_api::operations().iter().find(|o|o.id=="delete_v2_users_user_openid_messages_message_id").unwrap();
        let mut input=crate::qq_api::ExecuteBody{parameters:BTreeMap::from([("user_openid".into(),"Synthetic_User".into()),("message_id".into(),"ROBOT1.0_AB!CD+EF/GH==".into())]),acknowledge_effects:true,..Default::default()};
        let(path,query)=crate::qq_api::build_request(op,&input).unwrap();
        let expected="/v2/users/Synthetic_User/messages/ROBOT1.0_AB%21CD%2BEF%2FGH%3D%3D";
        assert_eq!(path,expected);
        let reply=request_http(&base,"synthetic-app","synthetic-test-token",reqwest::Method::DELETE,&path,&query,None).await.unwrap();
        assert!(reply.accepted());
        let recorded=receipts.lock().unwrap().clone();
        assert_eq!(recorded.len(),1);assert_eq!(recorded[0].path,expected);assert_eq!(recorded[0].path.split('/').count(),6);
        input.parameters.insert("message_id".into(),"a..b".into());
        assert_eq!(crate::qq_api::build_request(op,&input).unwrap().0,"/v2/users/Synthetic_User/messages/a..b");
        for invalid in ["../other","abc/../../users/@me","https://attacker.invalid/path","//attacker.invalid/path","encoded%2Fpath","bad\nheader","."]{
            input.parameters.insert("message_id".into(),invalid.into());
            assert!(crate::qq_api::build_request(op,&input).is_err(),"{invalid}");
        }
        assert_eq!(receipts.lock().unwrap().len(),1,"invalid input cannot create an additional HTTP request");
        server.abort();
    }
}
