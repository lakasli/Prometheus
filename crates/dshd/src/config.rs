use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    pub assistant: AssistantConfig,
    pub model: ModelConfig,
    pub qq: QqConfig,
    pub reply: crate::chat_reply::ReplyConfig,
    pub speech: crate::speech::SpeechConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AssistantConfig {
    pub name: String,
    pub system_prompt: String,
    pub max_concurrent_runs: usize,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    pub base_url: String,
    pub model: String,
    pub credential_ref: Option<String>,
    /// Administrator-confirmed OpenAI-compatible image generation endpoint.
    /// Model names or prompt claims never enable this capability implicitly.
    #[serde(skip_serializing_if = "is_false")]
    pub image_generation: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QqConfig {
    pub enabled: bool,
    pub app_id: String,
    pub credential_ref: Option<String>,
    pub transport: String,
}

impl Default for AssistantConfig {
    fn default() -> Self {
        let card = crate::character::built_in_personality_card();
        Self {
            name: card["data"]["name"].as_str().unwrap().into(),
            system_prompt: crate::character::compile(&card, "用户")
                .expect("bundled personality must compile")
                .0,
            max_concurrent_runs: 2,
            timeout_seconds: 120,
        }
    }
}
impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-chat".into(),
            credential_ref: None,
            image_generation: false,
        }
    }
}
impl Default for QqConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: String::new(),
            credential_ref: None,
            transport: "websocket".into(),
        }
    }
}

impl RuntimeConfig {
    pub fn validate(&self, allow_private: bool) -> Vec<Value> {
        let mut errors = Vec::new();
        let mut add =
            |field: &str, message: &str| errors.push(json!({"field":field,"message":message}));
        if self.assistant.name.trim().is_empty() || self.assistant.name.len() > 128 {
            add("assistant.name", "名称不能为空，且不得超过128字节");
        }
        if self.assistant.system_prompt.len() > 32000 {
            add("assistant.system_prompt", "提示词不得超过32000字节");
        }
        if !(1..=2).contains(&self.assistant.max_concurrent_runs) {
            add("assistant.max_concurrent_runs", "树莓派初始并发上限为1到2");
        }
        if !(5..=300).contains(&self.assistant.timeout_seconds) {
            add("assistant.timeout_seconds", "超时必须在5到300秒之间");
        }
        if self.model.model.trim().is_empty() || self.model.model.len() > 200 {
            add("model.model", "请填写有效模型名称");
        }
        if validate_model_url(&self.model.base_url, allow_private).is_err() {
            add(
                "model.base_url",
                "仅允许不含账户、查询参数或片段的HTTPS模型地址；私网地址需在部署时显式授权",
            );
        }
        if self.qq.enabled
            && (self.qq.app_id.is_empty()
                || !self.qq.app_id.bytes().all(|c| c.is_ascii_digit())
                || self.qq.app_id.len() > 32)
        {
            add("qq.app_id", "启用QQ时需要填写数字AppID");
        }
        if self.qq.enabled && self.qq.credential_ref.is_none() {
            add("qq.credential_ref", "启用QQ时需要设置AppSecret");
        }
        if !["websocket", "webhook"].contains(&self.qq.transport.as_str()) {
            add("qq.transport", "QQ传输方式必须为websocket或webhook");
        }
        errors.extend(self.reply.validate());
        errors.extend(self.speech.validate());
        errors
    }
}

pub fn validate_model_url(value: &str, allow_private: bool) -> anyhow::Result<Url> {
    let url = Url::parse(value)?;
    anyhow::ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid endpoint"
    );
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("missing host"))?;
    anyhow::ensure!(
        url.scheme() == "https" || (allow_private && url.scheme() == "http"),
        "HTTPS required"
    );
    if !allow_private {
        anyhow::ensure!(
            !host.eq_ignore_ascii_case("localhost")
                && !host.ends_with(".localhost")
                && host
                    .parse::<std::net::IpAddr>()
                    .map(public_ip)
                    .unwrap_or(true),
            "private endpoint denied"
        );
    }
    Ok(url)
}

pub fn public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => {
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.is_multicast()
                || v.octets()[0] == 0
                || v.octets()[0] >= 240
                || (v.octets()[0] == 100 && (64..=127).contains(&v.octets()[1])))
        }
        std::net::IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return public_ip(v4.into());
            }
            !(v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (v.segments()[0] & 0xfe00) == 0xfc00
                || (v.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}
