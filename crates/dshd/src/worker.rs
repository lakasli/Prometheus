use crate::{
    character::{self, StaticSnapshot},
    config::{public_ip, validate_model_url, RuntimeConfig},
    error::{ApiError, ApiResult},
    state::{now, AppState},
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{net::SocketAddr, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyTarget {
    pub kind: String,
    pub target: String,
    pub message_id: String,
    pub account_id: String,
    pub credential_ref: String,
    pub deadline: i64,
}

pub fn enqueue(
    state: &AppState,
    conversation: &str,
    owner: &str,
    client_id: &str,
    input: &str,
    reply: Option<ReplyTarget>,
) -> ApiResult<String> {
    if input.trim().is_empty()
        || input.len() > 16000
        || client_id.is_empty()
        || client_id.len() > 256
    {
        return Err(ApiError::bad(
            "invalid_message",
            "消息须为1到16000字节，且需要有效的客户端消息ID",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let result=state.db(|db| {
        let tx=db.transaction()?;
        if let Some((existing,content))=tx.query_row("SELECT id,input FROM runs WHERE conversation_id=?1 AND client_message_id=?2",params![conversation,client_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()? {
            return Ok(if content==input {Some(existing)} else {None});
        }
        let pending:i64=tx.query_row("SELECT count(*) FROM runs WHERE status='queued'",[],|r|r.get(0))?;
        let per_conversation:i64=tx.query_row("SELECT count(*) FROM runs WHERE conversation_id=?1 AND status IN ('queued','running')",[conversation],|r|r.get(0))?;
        if pending>=500||per_conversation>=10{return Ok(None);}
        let (config_revision,config)=character::current_config_in(&tx)?;
        let user_name=tx.query_row("SELECT username FROM admins WHERE id=?1",[owner],|r|r.get::<_,String>(0)).optional()?.unwrap_or_else(||"用户".into());
        let snapshot=character::freeze_for_run(&tx,conversation,config_revision,&config,&user_name)?;
        tx.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,reply_json,status,created_at,updated_at,character_snapshot_json,character_digest,compiler_version) VALUES(?1,?2,?3,?4,?5,?6,?7,'queued',?8,?8,?9,?10,?11)",params![id,conversation,owner,client_id,input,serde_json::to_string(&config)?,reply.map(|r|serde_json::to_string(&r)).transpose()?,now(),snapshot.as_ref().map(serde_json::to_string).transpose()?,snapshot.as_ref().map(|s|&s.projection_digest),snapshot.as_ref().map(|s|&s.compiler_version)])?;
        tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'user',?4,?5)",params![uuid::Uuid::new_v4().to_string(),conversation,id,input,now()])?;
        tx.commit()?;Ok(Some(id))
    })?;
    let result = result.ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "queue_or_idempotency_conflict",
            "会话队列已满或相同消息ID对应不同内容",
        )
    })?;
    state.0.notify.notify_waiters();
    Ok(result)
}

struct Claimed {
    id: String,
    conversation: String,
    config: RuntimeConfig,
    reply: Option<ReplyTarget>,
    character: Option<StaticSnapshot>,
}
fn claim(state: &AppState) -> ApiResult<Option<Claimed>> {
    let (_, config) = state.current_config()?;
    state.db(|db| {
        let tx=db.transaction()?;
        let running:i64=tx.query_row("SELECT count(*) FROM runs WHERE status='running'",[],|r|r.get(0))?;
        if running>=config.assistant.max_concurrent_runs.min(2) as i64{return Ok(None);}
        let item=tx.query_row("SELECT r.id,r.conversation_id,r.config_json,r.reply_json,r.character_snapshot_json FROM runs r WHERE r.status='queued' AND NOT EXISTS(SELECT 1 FROM runs other WHERE other.conversation_id=r.conversation_id AND other.status='running') ORDER BY r.created_at,r.rowid LIMIT 1",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?))).optional()?;
        let Some((id,conversation,config,reply,character))=item else{return Ok(None)};
        tx.execute("UPDATE runs SET status='running',updated_at=?1 WHERE id=?2 AND status='queued'",params![now(),id])?;
        tx.commit()?;
        Ok(Some(Claimed{id,conversation,config:serde_json::from_str(&config)?,reply:reply.map(|s|serde_json::from_str(&s)).transpose()?,character:character.map(|s|serde_json::from_str(&s)).transpose()?}))
    })
}

pub async fn run_worker(state: AppState, cancellation: CancellationToken) {
    loop {
        if cancellation.is_cancelled() {
            break;
        }
        match claim(&state) {
            Ok(Some(item)) => process(&state, item, &cancellation).await,
            Ok(None) => {
                tokio::select! {_=cancellation.cancelled()=>break,_=state.0.notify.notified()=>{},_=tokio::time::sleep(Duration::from_millis(500))=>{}}
            }
            Err(_) => {
                tracing::warn!("run queue unavailable");
                tokio::select! {_=cancellation.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
            }
        }
    }
}

async fn process(state: &AppState, item: Claimed, cancel: &CancellationToken) {
    let prepared = prepare_messages(state, &item);
    let result = match prepared {
        Ok(messages) => {
            let cancelled = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let status = state.db(|db| {
                        Ok(db.query_row(
                            "SELECT status FROM runs WHERE id=?1",
                            [&item.id],
                            |r| r.get::<_, String>(0),
                        )?)
                    });
                    if !matches!(status,Ok(s) if s=="running") {
                        break;
                    }
                }
            };
            tokio::select! {
                result=model_request(state,&item.config,messages,1024)=>result,
                _=cancel.cancelled()=>Err("服务正在停止，任务结果未确认".into()),
                _=cancelled=>Err("任务已取消".into()),
            }
        }
        Err(_) => Err("准备或持久化会话上下文失败；未发送模型请求".into()),
    };
    let saved=state.db(|db| {
        let tx=db.transaction()?;
        let status:String=tx.query_row("SELECT status FROM runs WHERE id=?1",[&item.id],|r|r.get(0))?;
        if status!="running" {return Ok(());}
        match result {
            Ok(content)=> {
                tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'assistant',?4,?5)",params![uuid::Uuid::new_v4().to_string(),item.conversation,item.id,content,now()])?;
                tx.execute("UPDATE runs SET status='completed',response=?1,updated_at=?2 WHERE id=?3",params![content,now(),item.id])?;
                if let Some(target)=item.reply {tx.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES(?1,?2,?3,?4,?5,?6,'queued',?7,?8)",params![uuid::Uuid::new_v4().to_string(),item.id,target.account_id,target.credential_ref,serde_json::to_string(&target)?,content,target.deadline,now()])?;}
            },
            Err(error)=> {tx.execute("UPDATE runs SET status=?1,error=?2,updated_at=?3 WHERE id=?4",params![if cancel.is_cancelled(){"interrupted"}else{"failed"},error,now(),item.id])?;}
        }
        tx.commit()?;Ok(())
    });
    if saved.is_err() {
        tracing::error!(run_id=%item.id,"failed to persist run result");
    }
    state.0.notify.notify_waiters();
}

// Freeze complete history only after the preceding run in this conversation has settled.
// Static persona/model inputs were already frozen atomically at enqueue time.
fn prepare_messages(state: &AppState, item: &Claimed) -> ApiResult<Vec<Value>> {
    state.db(|db| {
        let tx = db.transaction()?;
        if let Some(text) = tx.query_row("SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?1", [&item.id], |r|r.get::<_,String>(0)).optional()? {
            return Ok(serde_json::from_str(&text)?);
        }
        let status:String=tx.query_row("SELECT status FROM runs WHERE id=?1",[&item.id],|r|r.get(0))?;
        anyhow::ensure!(status=="running", "run is no longer active");
        let history = {
            let mut stmt=tx.prepare("SELECT run_id,role,content FROM (SELECT r.rowid AS run_order,m.run_id,m.role,m.content FROM messages m JOIN runs r ON r.id=m.run_id WHERE m.conversation_id=?1 AND r.run_kind='model' AND r.rowid<=(SELECT rowid FROM runs WHERE id=?2) AND (r.status='completed' OR r.id=?2) ORDER BY r.rowid DESC,CASE m.role WHEN 'user' THEN 0 ELSE 1 END DESC LIMIT 24) ORDER BY run_order,CASE role WHEN 'user' THEN 0 ELSE 1 END")?;
            let rows=stmt.query_map(params![item.conversation,item.id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?;
            rows.collect::<Result<Vec<_>,_>>()?
        };
        let mut groups:Vec<(String,Vec<Value>,usize)>=Vec::new();
        for (run_id,role,content) in history {
            if groups.last().is_none_or(|g|g.0!=run_id) { groups.push((run_id,Vec::new(),0)); }
            let group=groups.last_mut().expect("group just inserted");
            group.2+=content.len();
            group.1.push(json!({"role":role,"content":content}));
        }
        // LIMIT may cut the oldest pair. Never send an orphan assistant turn.
        groups.retain(|(id,messages,_)|id==&item.id || (messages.len()==2 && messages[0]["role"]=="user" && messages[1]["role"]=="assistant"));
        let mut bytes:usize=groups.iter().map(|g|g.2).sum();
        while bytes>64000 && groups.len()>1 {bytes-=groups.remove(0).2;}
        let system=item.character.as_ref().map(|s|s.system_prompt.as_str()).unwrap_or(&item.config.assistant.system_prompt);
        let messages=loop {
            let mut messages=vec![json!({"role":"system","content":system})];
            for (_,items,_) in &groups {messages.extend(items.iter().cloned());}
            if serde_json::to_vec(&messages)?.len()<=96000 {break messages;}
            anyhow::ensure!(groups.len()>1,"current input and persona exceed request budget");
            groups.remove(0);
        };
        let text=serde_json::to_string(&messages)?;
        let compiler=item.character.as_ref().map(|s|s.compiler_version.as_str()).unwrap_or("legacy-m1");
        tx.execute("INSERT INTO run_prompt_snapshots(run_id,messages_json,digest,compiler_version,created_at) VALUES(?1,?2,?3,?4,?5)",params![item.id,text,character::digest(text.as_bytes()),compiler,now()])?;
        tx.commit()?;
        Ok(messages)
    })
}

pub async fn model_client(
    base_url: &str,
    allow_private: bool,
    timeout: u64,
) -> Result<(reqwest::Client, String), String> {
    let url = validate_model_url(base_url, allow_private)
        .map_err(|_| "模型地址未通过安全校验".to_string())?;
    let host = url.host_str().ok_or("模型地址缺少主机")?;
    let port = url.port_or_known_default().ok_or("模型地址端口无效")?;
    let addresses: Vec<SocketAddr> = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .map_err(|_| "模型DNS解析超时")?
    .map_err(|_| "模型DNS解析失败")?
    .collect();
    if addresses.is_empty() || (!allow_private && addresses.iter().any(|a| !public_ip(a.ip()))) {
        return Err("模型DNS地址不被允许；私网访问需部署管理员显式授权".into());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(timeout.min(300)))
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|_| "无法创建模型HTTP客户端")?;
    Ok((
        client,
        format!("{}/chat/completions", base_url.trim_end_matches('/')),
    ))
}

pub async fn test_model(state: &AppState, config: &RuntimeConfig) -> Result<(), String> {
    model_request(
        state,
        config,
        vec![json!({"role":"user","content":"Reply only: OK"})],
        8,
    )
    .await
    .map(|_| ())
}
async fn model_request(
    state: &AppState,
    config: &RuntimeConfig,
    messages: Vec<Value>,
    max_tokens: usize,
) -> Result<String, String> {
    let credential = config
        .model
        .credential_ref
        .as_ref()
        .ok_or("请先设置模型API密钥")?;
    let key = state
        .secret(credential, "model_api_key")
        .map_err(|_| "模型凭证不可用")?;
    let (client, url) = model_client(
        &config.model.base_url,
        state.0.options.allow_private_model,
        config.assistant.timeout_seconds,
    )
    .await?;
    let mut response=client.post(url).bearer_auth(key).json(&json!({"model":config.model.model,"messages":messages,"stream":false,"max_tokens":max_tokens})).send().await.map_err(|error|if error.is_timeout(){"模型请求超时"}else{"模型网络请求失败"}.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "模型服务返回HTTP {}；请检查模型名、凭证与额度",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "模型响应读取失败")? {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err("模型响应超过1MiB限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "模型返回了无效JSON")?;
    let content = value["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("模型未返回文字回答")?;
    if content.is_empty() || content.len() > 65536 {
        return Err("模型文字为空或超过64KiB限制".into());
    }
    Ok(content.to_string())
}
