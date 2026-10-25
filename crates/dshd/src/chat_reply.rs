//! A model proposes presentation only. It cannot select URLs, files, recipients,
//! credentials or permissions. The host validates and materializes every item.
use crate::config::RuntimeConfig;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReplyConfig {
    pub min_interval_ms: u64,
    pub max_interval_ms: u64,
    pub max_items: usize,
}
impl Default for ReplyConfig {
    fn default() -> Self {
        Self {
            min_interval_ms: 1500,
            max_interval_ms: 8000,
            max_items: 4,
        }
    }
}
impl ReplyConfig {
    pub fn validate(&self) -> Vec<Value> {
        let mut errors = Vec::new();
        if !(1000..=10000).contains(&self.min_interval_ms) {
            errors.push(json!({"field":"reply.min_interval_ms","message":"最小发送间隔须为1000到10000毫秒"}));
        }
        if self.max_interval_ms < self.min_interval_ms || self.max_interval_ms > 30000 {
            errors.push(json!({"field":"reply.max_interval_ms","message":"最大发送间隔须不小于最小间隔，且不超过30000毫秒"}));
        }
        if !(1..=4).contains(&self.max_items) {
            errors.push(json!({"field":"reply.max_items","message":"每次回复须为1到4条，遵守QQ C2C回复次数上限"}));
        }
        errors
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyItem {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub delay_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyPlan {
    pub version: u32,
    pub normalized: bool,
    pub items: Vec<ReplyItem>,
}
impl ReplyPlan {
    pub fn readable(&self) -> String {
        self.items
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS run_reply_plans(run_id TEXT PRIMARY KEY REFERENCES runs(id),plan_json TEXT NOT NULL,created_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS outbox_reply_parts(outbox_id TEXT NOT NULL REFERENCES outbox(id),seq INTEGER NOT NULL CHECK(seq BETWEEN 1 AND 4),payload_json TEXT NOT NULL,delay_ms INTEGER NOT NULL,status TEXT NOT NULL DEFAULT 'queued',due_ms INTEGER NOT NULL DEFAULT 0,platform_message_id TEXT,error TEXT,finished_ms INTEGER,PRIMARY KEY(outbox_id,seq));
        CREATE TABLE IF NOT EXISTS reply_account_pacing(account_id TEXT PRIMARY KEY,next_ms INTEGER NOT NULL);
        UPDATE outbox_reply_parts SET status='unknown',error='服务重启时发送结果未知' WHERE status='sending';")?;
    Ok(())
}

/// Close quotes belong to the preceding sentence, and decimals do not split.
pub fn sentences(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.trim().chars().collect();
    let mut items = Vec::new();
    let mut current = String::new();
    let mut boundary = false;
    for (index, &ch) in chars.iter().enumerate() {
        if boundary
            && !matches!(
                ch,
                '。' | '！'
                    | '？'
                    | '!'
                    | '?'
                    | '.'
                    | '”'
                    | '’'
                    | '"'
                    | '\''
                    | '）'
                    | ')'
                    | '】'
                    | ']'
            )
        {
            if !current.trim().is_empty() {
                items.push(current.trim().to_owned());
            }
            current.clear();
            boundary = false;
        }
        current.push(ch);
        let decimal = ch == '.'
            && index > 0
            && chars[index - 1].is_ascii_digit()
            && chars.get(index + 1).is_some_and(char::is_ascii_digit);
        let period = ch == '.'
            && !decimal
            && chars
                .get(index + 1)
                .is_none_or(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '”' | '’'));
        if matches!(ch, '。' | '！' | '？' | '!' | '?') || period {
            boundary = true;
        }
    }
    if !current.trim().is_empty() {
        items.push(current.trim().to_owned());
    }
    items
}

pub fn parse(raw: &str, config: &ReplyConfig) -> Result<ReplyPlan, String> {
    if raw.len() > 65536 || raw.trim().is_empty() {
        return Err("回复内容为空或过长".into());
    }
    let trimmed = raw.trim();
    let fenced = trimmed
        .strip_prefix("```json\n")
        .or_else(|| trimmed.strip_prefix("```\n"))
        .and_then(|s| s.strip_suffix("```"));
    let (mut items, mut normalized) = if trimmed.starts_with('[') || fenced.is_some() {
        (
            serde_json::from_str::<Vec<ReplyItem>>(fenced.unwrap_or(trimmed).trim())
                .map_err(|_| "模型回复未符合逐句JSON列表契约".to_owned())?,
            fenced.is_some(),
        )
    } else {
        // Older compatible providers may ignore the protocol. Normalize their
        // text; never forward a multi-sentence block as a single channel item.
        if trimmed.starts_with('{') {
            return Err("模型应返回JSON列表，不能返回对象或工具指令".into());
        }
        (
            sentences(trimmed)
                .into_iter()
                .map(|text| ReplyItem {
                    kind: "text".into(),
                    text,
                    delay_ms: 0,
                    asset_id: None,
                    prompt: None,
                })
                .collect(),
            true,
        )
    };
    if items.is_empty() || items.len() > config.max_items.min(4) {
        return Err("模型回复超过逐句消息数量限制，请缩短回答".into());
    }
    for (index, item) in items.iter_mut().enumerate() {
        item.text = item.text.trim().to_owned();
        if !["text", "voice", "image", "video"].contains(&item.kind.as_str())
            || item.text.is_empty()
            || item.text.chars().count() > 1000
            || item.text.contains('\0')
        {
            return Err("回复类型、单句长度或内容无效".into());
        }
        let split = sentences(&item.text);
        if split.len() != 1 {
            return Err("每个回复对象只允许一句话".into());
        }
        if let Some(id) = &item.asset_id {
            if uuid::Uuid::parse_str(id).is_err() {
                return Err("媒体制品引用无效".into());
            }
        }
        if item.kind == "text" && (item.asset_id.is_some() || item.prompt.is_some())
            || item.kind == "voice" && item.prompt.is_some()
            || item.kind == "video" && (item.asset_id.is_none() || item.prompt.is_some())
            || item.kind == "image" && (item.asset_id.is_some() == item.prompt.is_some())
        {
            return Err("媒体回复须使用已授权制品；生图只允许单独的绘画需求".into());
        }
        if item
            .prompt
            .as_ref()
            .is_some_and(|s| s.trim().is_empty() || s.len() > 4000 || s.contains('\0'))
        {
            return Err("绘画需求无效".into());
        }
        let delay = if index == 0 {
            0
        } else {
            item.delay_ms
                .clamp(config.min_interval_ms, config.max_interval_ms)
        };
        normalized |= delay != item.delay_ms;
        item.delay_ms = delay;
    }
    Ok(ReplyPlan {
        version: 1,
        normalized,
        items,
    })
}

pub fn prompt(config: &RuntimeConfig, assets: &Value) -> String {
    format!("\n宿主回复协议（优先于表达偏好）：仅返回JSON数组，不加代码围栏；1到{}个对象，每个对象仅一句话，简洁自然，不把多句话塞进同一对象。对象字段为type、text、delay_ms，可选asset_id或prompt。type=text普通文字；type=voice朗读text，仅用户要求语音且speech_available=true时可用；type=image使用已列出的图片asset_id或在image_generation_available=true时提供prompt请求宿主绘画；type=video仅使用已列出的视频asset_id。不得编造文件、URL、制品或权限，不输出shell/路径/接收者/凭证。第一项delay_ms=0，后续{}到{}毫秒，由宿主强制节流。没有可用媒体时诚实说明并使用text。例：[{{\"type\":\"text\",\"text\":\"我在这里。\",\"delay_ms\":0}},{{\"type\":\"text\",\"text\":\"你今天过得怎么样？\",\"delay_ms\":2000}}]。宿主当前能力及当前会话允许引用的制品：{}",config.reply.max_items,config.reply.min_interval_ms,config.reply.max_interval_ms,assets)
}

pub fn store_in(db: &Connection, run: &str, plan: &ReplyPlan) -> anyhow::Result<()> {
    require_reader(db, 9)?;
    db.execute(
        "INSERT INTO run_reply_plans(run_id,plan_json,created_at) VALUES(?1,?2,?3)",
        params![run, serde_json::to_string(plan)?, crate::state::now()],
    )?;
    Ok(())
}
pub fn get_in(db: &Connection, run: &str) -> anyhow::Result<Option<ReplyPlan>> {
    let raw = db
        .query_row(
            "SELECT plan_json FROM run_reply_plans WHERE run_id=?1",
            [run],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    raw.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}
pub fn queue_parts_in(db: &Connection, outbox: &str, plan: &ReplyPlan) -> anyhow::Result<()> {
    require_reader(db, 9)?;
    for (index, item) in plan.items.iter().enumerate() {
        db.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms) VALUES(?1,?2,?3,?4)",params![outbox,index+1,serde_json::to_string(item)?,item.delay_ms])?;
    }
    Ok(())
}
pub fn require_reader(db: &Connection, version: i64) -> anyhow::Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO dsh_schema_migrations(version,created_at) VALUES(?1,?2)",
        params![version, crate::state::now()],
    )?;
    Ok(())
}

pub async fn materialize(
    state: &crate::state::AppState,
    config: &RuntimeConfig,
    owner: &str,
    conversation: &str,
    plan: &mut ReplyPlan,
) -> Result<(), String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    for item in &mut plan.items {
        let result: Result<String, String> = match item.kind.as_str() {
            "text" => continue,
            "voice" => {
                if let Some(id) = &item.asset_id {
                    crate::speech::read_asset(state, owner, id)
                        .map_err(|_| "语音制品已过期或未获授权".to_owned())
                        .and_then(|(m, _)| {
                            if m.conversation_id.as_deref() == Some(conversation) {
                                Ok(id.clone())
                            } else {
                                Err("语音制品不属于当前会话".into())
                            }
                        })
                } else {
                    crate::speech::synthesize(
                        state,
                        &config.speech,
                        owner,
                        Some(conversation),
                        &item.text,
                    )
                    .await
                    .map(|asset| asset.id)
                    .map_err(|_| "本地语音合成暂不可用，已保留文字".into())
                }
            }
            "image" if item.prompt.is_some() => {
                let prompt = item.prompt.as_deref().unwrap_or_default();
                match crate::skill_drawing::generate(state, config, prompt).await {
                    Ok(markdown) => {
                        let encoded = markdown
                            .strip_prefix("![绘画](data:image/png;base64,")
                            .and_then(|s| s.strip_suffix(')'))
                            .ok_or("生图制品格式无效".to_owned())?;
                        let bytes = STANDARD
                            .decode(encoded)
                            .map_err(|_| "生图制品编码无效".to_owned())?;
                        crate::media_assets::store(state, owner, conversation, "image", bytes)
                            .map(|a| a.id)
                            .map_err(|_| "生图制品保存失败".into())
                    }
                    Err(_) => Err("当前绘画服务不可用，无法生成图片".into()),
                }
            }
            "image" | "video" => crate::media_assets::read(
                state,
                owner,
                conversation,
                item.asset_id.as_deref().unwrap_or_default(),
            )
            .map_err(|_| "媒体制品已过期或不属于当前会话".to_owned())
            .and_then(|(a, _)| {
                if a.kind == item.kind {
                    Ok(a.id)
                } else {
                    Err("媒体类型不匹配".into())
                }
            }),
            _ => Err("不支持此回复类型".into()),
        };
        match result {
            Ok(id) => {
                item.asset_id = Some(id);
                item.prompt = None;
            }
            Err(error) => {
                if item.kind != "voice" {
                    item.text = format!("{error}。");
                }
                item.kind = "text".into();
                item.asset_id = None;
                item.prompt = None;
                plan.normalized = true;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splits_chinese_english_quotes_and_decimal_without_bursts() {
        assert_eq!(
            sentences("你好！今天是3.14。\"Good day.\" Really?"),
            vec!["你好！", "今天是3.14。\"", "Good day.\"", "Really?"]
        );
        assert_eq!(
            sentences("The value is 3.14. Next sentence."),
            vec!["The value is 3.14.", "Next sentence."]
        );
    }
    #[test]
    fn clamps_intervals_and_rejects_untrusted_actions() {
        let c = ReplyConfig::default();
        let p=parse(r#"[{"type":"text","text":"第一句。","delay_ms":0},{"type":"text","text":"第二句。","delay_ms":1}]"#,&c).unwrap();
        assert_eq!(p.items[1].delay_ms, 1500);
        assert!(parse(r#"[{"type":"text","text":"一句。两句。"}]"#, &c).is_err());
        assert!(parse(
            r#"[{"type":"image","text":"图片。","url":"http://localhost"}]"#,
            &c
        )
        .is_err());
        assert!(parse(
            r#"[{"type":"video","text":"视频。","asset_id":"/etc/passwd"}]"#,
            &c
        )
        .is_err());
        assert!(parse(
            r#"[{"type":"voice","text":"你好。","recipient":"other"}]"#,
            &c
        )
        .is_err());
        assert!(parse("一。二。三。四。五。", &c).is_err());
        assert_eq!(parse("一句。两句。", &c).unwrap().items.len(), 2);
    }
}
