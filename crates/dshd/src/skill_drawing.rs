//! Native drawing uses only the administrator-configured model endpoint.
//! Provider URLs and metadata are never embedded, fetched or sent to the UI.
use crate::{config::RuntimeConfig, state::AppState, worker};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::io::{Cursor, Write};

const MAX_PNG_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 6 * 1024 * 1024;
const MAX_DECODE_BYTES: usize = 32 * 1024 * 1024;
const MAX_DIMENSION: u32 = 2048;
// Frozen Skill instructions (16 KiB) + user request (16 KiB) + labels.
const MAX_PROMPT_BYTES: usize = 33_000;
const INVALID_IMAGE: &str = "生图服务须返回受限大小的单张有效PNG（b64_json）";

struct BoundedPng(Vec<u8>);
impl Write for BoundedPng {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_PNG_BYTES {
            return Err(std::io::Error::other("drawing image exceeds bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Decode and re-encode pixels so PNG metadata never reaches the browser.
/// This path deliberately accepts PNG only, including for compatible providers.
fn canonical_png(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() > MAX_PNG_BYTES.div_ceil(3) * 4 {
        return Err(INVALID_IMAGE.into());
    }
    let bytes = STANDARD.decode(encoded).map_err(|_| INVALID_IMAGE)?;
    if bytes.len() > MAX_PNG_BYTES || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(INVALID_IMAGE.into());
    }
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: MAX_DECODE_BYTES,
        },
    );
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|_| INVALID_IMAGE)?;
    let width = reader.info().width;
    let height = reader.info().height;
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || reader.info().animation_control.is_some()
    {
        return Err(INVALID_IMAGE.into());
    }
    let size = reader
        .output_buffer_size()
        .filter(|size| *size <= MAX_DECODE_BYTES)
        .ok_or(INVALID_IMAGE)?;
    let mut pixels = vec![0; size];
    let frame = reader.next_frame(&mut pixels).map_err(|_| INVALID_IMAGE)?;
    reader.finish().map_err(|_| INVALID_IMAGE)?;
    if frame.width != width
        || frame.height != height
        || frame.bit_depth != png::BitDepth::Eight
        || frame.color_type == png::ColorType::Indexed
    {
        return Err(INVALID_IMAGE.into());
    }
    let mut output = BoundedPng(Vec::new());
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(frame.color_type);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|_| INVALID_IMAGE)?;
        writer
            .write_image_data(&pixels[..frame.buffer_size()])
            .map_err(|_| INVALID_IMAGE)?;
        writer.finish().map_err(|_| INVALID_IMAGE)?;
    }
    Ok(output.0)
}

/// The caller owns the existing native Run lease, timeout/cancel selection,
/// persistence and channel policy. Configured native failures remain failures.
pub async fn generate(
    state: &AppState,
    config: &RuntimeConfig,
    prompt: &str,
) -> Result<String, String> {
    if !config.model.image_generation {
        return Err("当前模型未启用原生生图能力".into());
    }
    if prompt.trim().is_empty() || prompt.len() > MAX_PROMPT_BYTES || prompt.contains('\0') {
        return Err("绘画要求与Skill步骤合计不得超过33000字节".into());
    }
    let credential = config
        .model
        .credential_ref
        .as_ref()
        .ok_or("请先设置模型API密钥")?;
    let key = state
        .secret(credential, "model_api_key")
        .map_err(|_| "模型凭证不可用")?;
    let (client, _) = worker::model_client(
        &config.model.base_url,
        state.0.options.allow_private_model,
        config.assistant.timeout_seconds,
    )
    .await?;
    let mut request = json!({
        "model": config.model.model,
        "prompt": prompt,
        "n": 1,
        "size": "1024x1024",
    });
    // GPT image models return b64_json and reject legacy response_format.
    // Naming affects wire compatibility only; the explicit flag above is final.
    if config.model.model.starts_with("gpt-image") || config.model.model == "chatgpt-image-latest" {
        request["output_format"] = json!("png");
    } else {
        request["response_format"] = json!("b64_json");
    }
    let endpoint = format!(
        "{}/images/generations",
        config.model.base_url.trim_end_matches('/')
    );
    let mut response = client
        .post(endpoint)
        .bearer_auth(key)
        .json(&request)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "生图请求超时"
            } else {
                "生图网络请求失败"
            }
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "生图服务返回HTTP {}；请检查生图能力、模型名、凭证与额度",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|bytes| bytes > MAX_RESPONSE_BYTES as u64)
    {
        return Err("生图响应超过6MiB限制".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "生图响应读取失败")? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("生图响应超过6MiB限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "生图服务返回了无效JSON")?;
    let images = value["data"].as_array().ok_or(INVALID_IMAGE)?;
    if images.len() != 1 {
        return Err(INVALID_IMAGE.into());
    }
    let encoded = images[0]["b64_json"].as_str().ok_or(INVALID_IMAGE)?;
    let canonical = canonical_png(encoded)?;
    Ok(format!(
        "![绘画](data:image/png;base64,{})",
        STANDARD.encode(canonical)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
        Json, Router,
    };
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tokio::{net::TcpListener, task::JoinHandle};

    #[derive(Clone)]
    struct Provider {
        mode: Arc<Mutex<String>>,
        requests: Arc<Mutex<Vec<Value>>>,
        png: String,
    }
    async fn serve(
        State(provider): State<Provider>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        assert_eq!(headers["authorization"], "Bearer synthetic-drawing-key");
        provider.requests.lock().unwrap().push(body);
        let mode = provider.mode.lock().unwrap().clone();
        match mode.as_str() {
            "error" => (StatusCode::UNAUTHORIZED, "synthetic-secret-provider-body").into_response(),
            "url" => Json(json!({"data":[{"url":"http://127.0.0.1/private"}]})).into_response(),
            "invalid" => Json(json!({"data":[{"b64_json":"not-an-image"}]})).into_response(),
            "multiple" => {
                Json(json!({"data":[{"b64_json":provider.png},{"b64_json":provider.png}]}))
                    .into_response()
            }
            "oversized" => "x".repeat(MAX_RESPONSE_BYTES + 1).into_response(),
            "json" => "{malformed".into_response(),
            _ => Json(json!({"data":[{"b64_json":provider.png}]})).into_response(),
        }
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .add_text_chunk("provider_note".into(), "metadata-must-disappear".into())
                .unwrap();
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&vec![100; width as usize * height as usize * 3])
                .unwrap();
        }
        bytes
    }

    async fn fixture() -> (TempDir, AppState, RuntimeConfig, Provider, JoinHandle<()>) {
        let directory = TempDir::new().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: directory.path().into(),
            origin: "http://127.0.0.1:18432".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: directory.path().join("web"),
        })
        .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        crate::state::private_write(
            &directory.path().join("secrets").join(&id),
            b"synthetic-drawing-key",
        )
        .unwrap();
        state
            .db(|db| {
                db.execute(
                    "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'model_api_key',0)",
                    [&id],
                )?;
                Ok(())
            })
            .unwrap();
        let provider = Provider {
            mode: Arc::new(Mutex::new("ok".into())),
            requests: Arc::new(Mutex::new(Vec::new())),
            png: STANDARD.encode(png(2, 2)),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/images/generations", post(serve))
            .with_state(provider.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = RuntimeConfig::default();
        config.model.base_url = format!("http://{address}/v1");
        config.model.credential_ref = Some(id);
        config.model.image_generation = true;
        (directory, state, config, provider, server)
    }

    #[tokio::test]
    async fn native_generation_uses_pinned_endpoint_and_strips_metadata() {
        let (_directory, state, mut config, provider, server) = fixture().await;
        let result = generate(&state, &config, "画一只蓝鲸").await.unwrap();
        let encoded = result
            .strip_prefix("![绘画](data:image/png;base64,")
            .unwrap()
            .strip_suffix(')')
            .unwrap();
        let bytes = STANDARD.decode(encoded).unwrap();
        assert!(!bytes
            .windows(b"metadata-must-disappear".len())
            .any(|part| part == b"metadata-must-disappear"));
        assert_eq!(canonical_png(encoded).unwrap(), bytes);
        let sent = provider.requests.lock().unwrap()[0].clone();
        assert_eq!(sent["response_format"], "b64_json");
        assert_eq!(sent["n"], 1);
        assert_eq!(sent["prompt"], "画一只蓝鲸");
        config.model.model = "gpt-image-1".into();
        generate(&state, &config, "画一片海").await.unwrap();
        let sent = provider.requests.lock().unwrap()[1].clone();
        assert!(sent.get("response_format").is_none());
        assert_eq!(sent["output_format"], "png");
        server.abort();
    }

    #[tokio::test]
    async fn disabled_and_invalid_native_responses_do_not_fallback_or_leak() {
        let (_directory, state, mut config, provider, server) = fixture().await;
        config.model.image_generation = false;
        assert!(generate(&state, &config, "draw").await.is_err());
        assert!(provider.requests.lock().unwrap().is_empty());
        config.model.image_generation = true;
        for mode in ["error", "url", "invalid", "multiple", "oversized", "json"] {
            *provider.mode.lock().unwrap() = mode.into();
            let error = generate(&state, &config, "draw").await.unwrap_err();
            assert!(!error.contains("synthetic-secret-provider-body"));
            assert!(!error.contains("synthetic-drawing-key"));
            assert!(!error.contains("127.0.0.1"));
        }
        server.abort();
    }

    #[test]
    fn canonicalization_limits_png_size_and_dimensions() {
        assert!(canonical_png(&STANDARD.encode(png(2049, 1))).is_err());
        assert!(canonical_png(&"A".repeat(MAX_PNG_BYTES.div_ceil(3) * 4 + 1)).is_err());
        assert!(canonical_png(&STANDARD.encode(b"<svg></svg>")).is_err());
        assert!(canonical_png("data:image/png;base64,YQ==").is_err());
        let mut writer = BoundedPng(vec![0; MAX_PNG_BYTES]);
        assert!(writer.write_all(&[1]).is_err());
    }

    #[test]
    fn old_model_configs_keep_native_generation_disabled() {
        let config: RuntimeConfig =
            serde_json::from_value(json!({"model":{"model":"gpt-image-1"}})).unwrap();
        assert!(!config.model.image_generation);
        assert!(serde_json::to_value(&config).unwrap()["model"]
            .get("image_generation")
            .is_none());
        let mut enabled = config;
        enabled.model.image_generation = true;
        assert_eq!(
            serde_json::to_value(&enabled).unwrap()["model"]["image_generation"],
            true
        );
    }
}
