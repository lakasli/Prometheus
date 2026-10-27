//! Durable ordered QQ delivery. Each uncertain request is terminal; later parts
//! remain blocked. Intervals start at actual completion, never model completion.
use crate::{
    chat_reply::ReplyItem,
    state::{now, AppState},
    worker::ReplyTarget,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

type SendResult = Result<(String, Option<String>), (bool, String)>;

#[cfg(test)]
#[path = "queued_context_delivery_tests.rs"]
mod queued_context_tests;

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
#[derive(Debug)]
struct Delivery {
    id: String,
    seq: Option<i64>,
    target: ReplyTarget,
    content: String,
    owner: String,
    conversation: String,
}
fn claim_in(db: &mut Connection, at: i64) -> anyhow::Result<Option<Delivery>> {
    let tx = db.transaction()?;
    // Expire before selecting: a busy unrelated conversation must not keep a
    // dead predecessor fencing this conversation indefinitely.
    tx.execute("UPDATE outbox SET status='expired',error='回复截止时间已过，未发送' WHERE status='queued' AND deadline<?1",[now()])?;
    tx.execute("UPDATE outbox_reply_parts SET status=(SELECT status FROM outbox WHERE id=outbox_reply_parts.outbox_id),error=(SELECT error FROM outbox WHERE id=outbox_reply_parts.outbox_id) WHERE status='queued' AND EXISTS(SELECT 1 FROM outbox o WHERE o.id=outbox_reply_parts.outbox_id AND o.status IN ('expired','cancelled'))",[])?;
    crate::conversation_context::fence_queued_delivery_candidates_in(&tx, true)?;
    let rest = crate::rest_publication::schema_present_in(&tx)?;
    let ack = if rest {
        "EXISTS(SELECT 1 FROM rest_ack_permits ack WHERE ack.source_run_id=r.id)"
    } else {
        "0"
    };
    let query = format!("SELECT o.id,o.target_json,o.content,r.owner_id,r.conversation_id,p.seq,p.payload_json,r.id,o.deadline
        FROM outbox o JOIN runs r ON r.id=o.run_id
        LEFT JOIN outbox_reply_parts p ON p.outbox_id=o.id AND p.seq=(SELECT min(q.seq) FROM outbox_reply_parts q WHERE q.outbox_id=o.id AND q.status='queued')
        WHERE o.status='queued' AND o.deadline>=?1 AND (p.seq IS NULL OR p.due_ms<=?2)
        AND o.id NOT IN(SELECT value FROM json_each(?3))
        AND NOT EXISTS(SELECT 1 FROM outbox_reply_parts u WHERE u.outbox_id=o.id AND u.status IN('unknown','failed','sending'))
        AND NOT EXISTS(SELECT 1 FROM reply_account_pacing a WHERE a.account_id=o.account_id AND a.next_ms>?2)
        AND NOT EXISTS(SELECT 1 FROM outbox older JOIN runs prior ON prior.id=older.run_id WHERE prior.conversation_id=r.conversation_id AND older.rowid<o.rowid AND (older.status IN('sending','unknown') OR EXISTS(SELECT 1 FROM outbox_reply_parts held WHERE held.outbox_id=older.id AND held.status IN('sending','unknown')) OR (older.status='queued' AND NOT({ack}))))
        ORDER BY CASE WHEN {ack} THEN 0 ELSE 1 END,o.rowid LIMIT 1");
    let mut skipped = Vec::<String>::new();
    // Consume stale queued records durably before considering a later answer;
    // cap work per tick so a large invalid queue cannot monopolize the DB lock.
    for _ in 0..64 {
        // The oldest unfinished message in one conversation fences newer answers.
        // Another conversation may continue while this one waits for its interval.
        let item = tx
            .query_row(
                &query,
                params![now(), at, serde_json::to_string(&skipped)?],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, i64>(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, target, content, owner, conversation, seq, payload, run, deadline)) = item
        else {
            tx.commit()?;
            return Ok(None);
        };
        skipped.push(id.clone());
        if !crate::conversation_context::claim_delivery_allowed_in(&tx, &run)? {
            continue;
        }
        let Some(revision) = crate::turn_control::active_revision_in(&tx, &run)? else {
            crate::turn_control::suppress_queued_in(&tx, &run, at)?;
            continue;
        };
        anyhow::ensure!(
            crate::turn_control::claim_allowed_in(&tx, &run, revision)?,
            "publication changed before send claim"
        );
        let target: ReplyTarget = serde_json::from_str(&target)?;
        if !crate::rest_publication::claim_allowed_in(
            &tx,
            &run,
            Some(deadline.min(target.deadline)),
            payload.as_deref().unwrap_or(&content),
        )? {
            continue;
        }
        tx.execute("UPDATE outbox SET status='sending' WHERE id=?1", [&id])?;
        if let Some(seq) = seq {
            tx.execute(
                "UPDATE outbox_reply_parts SET status='sending' WHERE outbox_id=?1 AND seq=?2",
                params![id, seq],
            )?;
        }
        tx.commit()?;
        return Ok(Some(Delivery {
            id,
            seq,
            target,
            content: payload.unwrap_or(content),
            owner,
            conversation,
        }));
    }
    tx.commit()?;
    Ok(None)
}
#[cfg(test)]
pub(crate) fn claim_id_for_rest_test(
    db: &mut Connection,
    at: i64,
) -> anyhow::Result<Option<String>> {
    Ok(claim_in(db, at)?.map(|item| item.id))
}
fn finish_in(
    db: &mut Connection,
    item: &Delivery,
    status: &str,
    message: Option<&str>,
    error: Option<&str>,
    at: i64,
    min_interval: u64,
) -> anyhow::Result<()> {
    let tx = db.transaction()?;
    let mut parent = status;
    if let Some(seq) = item.seq {
        tx.execute("UPDATE outbox_reply_parts SET status=?1,platform_message_id=?2,error=?3,finished_ms=?4 WHERE outbox_id=?5 AND seq=?6 AND status='sending'",params![status,message,error,at,item.id,seq])?;
        if status == "sent" {
            let next:Option<(i64,i64)>=tx.query_row("SELECT seq,delay_ms FROM outbox_reply_parts WHERE outbox_id=?1 AND status='queued' ORDER BY seq LIMIT 1",[&item.id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((next, delay)) = next {
                tx.execute(
                    "UPDATE outbox_reply_parts SET due_ms=?1 WHERE outbox_id=?2 AND seq=?3",
                    params![
                        at.saturating_add(delay.max(min_interval as i64)),
                        item.id,
                        next
                    ],
                )?;
                parent = "queued";
            }
        }
    }
    tx.execute("UPDATE outbox SET status=?1,platform_message_id=coalesce(?2,platform_message_id),error=?3 WHERE id=?4",params![parent,message,error,item.id])?;
    tx.execute("INSERT INTO reply_account_pacing(account_id,next_ms) VALUES(?1,?2) ON CONFLICT(account_id) DO UPDATE SET next_ms=max(reply_account_pacing.next_ms,excluded.next_ms)",params![item.target.account_id,at.saturating_add(min_interval as i64)])?;
    tx.commit()?;
    Ok(())
}

async fn send(state: &AppState, item: &Delivery) -> SendResult {
    let Some(seq) = item.seq else {
        return crate::qq::send_qq_payload(
            state,
            &item.target,
            &serde_json::json!({"type":"text","text":item.content}),
            1,
        )
        .await
        .map(|id| (id, None));
    };
    let part: ReplyItem =
        serde_json::from_str(&item.content).map_err(|_| (false, "无效回复对象".into()))?;
    if part.kind == "text" {
        return crate::qq::send_qq_payload(
            state,
            &item.target,
            &serde_json::to_value(&part).map_err(|_| (false, "无效回复对象".into()))?,
            seq as u32,
        )
        .await
        .map(|id| (id, None));
    }
    let bytes = (|| {
        let id = part
            .asset_id
            .as_ref()
            .ok_or((false, "媒体制品尚未准备".into()))?;
        if part.kind == "voice" {
            let (meta, bytes) = crate::speech::read_asset(state, &item.owner, id)
                .map_err(|_| (false, "语音制品已失效".into()))?;
            if meta.conversation_id.as_deref() != Some(&item.conversation) {
                return Err((false, "语音制品会话不匹配".into()));
            }
            Ok(bytes)
        } else {
            let (meta, bytes) =
                crate::media_assets::read(state, &item.owner, &item.conversation, id)
                    .map_err(|_| (false, "媒体制品不可用或归属不匹配".into()))?;
            if meta.kind != part.kind {
                return Err((false, "媒体类型不匹配".into()));
            }
            Ok(bytes)
        }
    })();
    let result = match bytes {
        Ok(bytes) => {
            crate::qq::send_media_bytes(
                state,
                &item.target,
                &part.kind,
                &bytes,
                &part.text,
                seq as u32,
            )
            .await
        }
        Err(error) => Err(error),
    };
    media_result(
        result,
        &part.kind,
        &part.text,
        seq as u32,
        |payload, seq| async move {
            crate::qq::send_qq_payload(state, &item.target, &payload, seq).await
        },
    )
    .await
}

async fn media_result<F, Fut>(
    result: crate::qq_media::DeliveryResult<String>,
    kind: &str,
    text: &str,
    seq: u32,
    send_text: F,
) -> SendResult
where
    F: FnOnce(Value, u32) -> Fut,
    Fut: std::future::Future<Output = crate::qq_media::DeliveryResult<String>>,
{
    match result {
        Ok(id) => Ok((id, None)),
        Err((false, cause)) => {
            let media_kind = match kind {
                "voice" => "voice",
                "image" => "image",
                "video" => "video",
                _ => "unsupported",
            };
            // This chain contains host-defined messages or HTTP/business
            // integers only. Never log a raw transport error, response or URL.
            tracing::warn!(
                channel = "qq",
                stage = "media_fallback",
                media_kind,
                seq,
                reason = %cause,
                "QQ media failed; attempting the text fallback"
            );
            let text = if kind == "voice" {
                format!("（语音暂不可用）{text}")
            } else {
                format!(
                    "{}发送暂不可用。",
                    if kind == "image" { "图片" } else { "视频" }
                )
            };
            // A failed text submission preserves its own uncertainty. The
            // earlier known media failure never lowers an unknown delivery.
            send_text(serde_json::json!({"type":"text","text":text}), seq)
                .await
                .map(|id| (id, Some(cause)))
        }
        Err(error) => Err(error),
    }
}

fn finish_sent_in(
    db: &mut Connection,
    item: &Delivery,
    message: &str,
    fallback_reason: Option<&str>,
    at: i64,
    interval: u64,
) -> anyhow::Result<()> {
    let error = fallback_reason.map(|reason| format!("媒体发送失败后已改发文字；原因：{reason}"));
    finish_in(
        db,
        item,
        "sent",
        Some(message),
        error.as_deref(),
        at,
        interval,
    )
}
pub async fn run(state: AppState, cancel: CancellationToken) {
    while !cancel.is_cancelled() {
        let next = state.db(|db| claim_in(db, now_ms()));
        let Ok(Some(item)) = next else {
            tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(200))=>{}};
            continue;
        };
        let result = tokio::select! {result=send(&state,&item)=>result,_=cancel.cancelled()=>Err((true,"服务停止时发送结果未知，未重发".into()))};
        let interval = state
            .current_config()
            .map(|(_, c)| c.reply.min_interval_ms)
            .unwrap_or(1500);
        let done = match result {
            Ok((id, fallback_reason)) => state.db(|db| {
                finish_sent_in(
                    db,
                    &item,
                    &id,
                    fallback_reason.as_deref(),
                    now_ms(),
                    interval,
                )
            }),
            Err((unknown, error)) => state.db(|db| {
                finish_in(
                    db,
                    &item,
                    if unknown { "unknown" } else { "failed" },
                    None,
                    Some(&error),
                    now_ms(),
                    interval,
                )
            }),
        };
        if done.is_err() {
            tracing::error!("QQ delivery result persistence unavailable");
        }
    }
}
pub fn status_in(db: &Connection, run: &str) -> anyhow::Result<Value> {
    let mut q=db.prepare("SELECT p.seq,p.status,p.due_ms,p.finished_ms,p.error FROM outbox_reply_parts p JOIN outbox o ON o.id=p.outbox_id WHERE o.run_id=?1 ORDER BY p.seq")?;
    let rows=q.query_map([run],|r|Ok(serde_json::json!({"seq":r.get::<_,i64>(0)?,"status":r.get::<_,String>(1)?,"due_ms":r.get::<_,i64>(2)?,"finished_ms":r.get::<_,Option<i64>>(3)?,"error":r.get::<_,Option<String>>(4)?})))?;
    Ok(serde_json::json!(rows.collect::<Result<Vec<_>, _>>()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    struct MockTransport {
        messages: Mutex<Vec<(String, Value)>>,
        text_status: StatusCode,
    }
    async fn rejected_upload(
        State(mock): State<Arc<MockTransport>>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        mock.messages.lock().unwrap().push(("upload".into(), body));
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"code":40093001,"message":"SYNTHETIC_PRIVATE_PLATFORM_BODY"})),
        )
    }
    async fn text_response(
        State(mock): State<Arc<MockTransport>>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        mock.messages.lock().unwrap().push(("text".into(), body));
        (
            mock.text_status,
            Json(serde_json::json!({"id":"synthetic-text-id"})),
        )
    }
    // A local HTTP boundary adapter for this module's result handling. It uses
    // the real platform response classifier, without replacing the production
    // QQ base URL or claiming to test the complete media upload implementation.
    async fn http_result(
        base: &str,
        path: &str,
        body: Value,
        stage: &str,
    ) -> crate::qq_media::DeliveryResult<String> {
        let response = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(4))
            .build()
            .unwrap()
            .post(format!("{base}{path}"))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let value: Value = response.json().await.unwrap();
        let reply = crate::qq_media::PlatformReply {
            http_status: status,
            business_code: value["code"].as_i64(),
            trace_id: None,
            value,
        };
        if reply.accepted() {
            Ok(reply.value["id"].as_str().unwrap().to_owned())
        } else {
            Err((reply.uncertain(), reply.status_text(stage)))
        }
    }

    #[tokio::test]
    async fn local_http_media_rejection_preserves_cause_and_same_seq_without_lowering_text_uncertainty(
    ) {
        for text_status in [
            StatusCode::OK,
            StatusCode::BAD_GATEWAY,
            StatusCode::BAD_REQUEST,
        ] {
            let mock = Arc::new(MockTransport {
                messages: Mutex::new(Vec::new()),
                text_status,
            });
            let router = Router::new()
                .route("/upload", post(rejected_upload))
                .route("/messages", post(text_response))
                .with_state(mock.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let dir = tempfile::tempdir().unwrap();
            let state = AppState::open(crate::state::Options {
                data_dir: dir.path().into(),
                origin: "http://localhost".into(),
                allow_private_model: false,
                cookie_secure: false,
                web_dir: dir.path().into(),
            })
            .unwrap();
            let target = ReplyTarget {
                kind: "c2c".into(),
                target: "synthetic-user".into(),
                message_id: "synthetic-input".into(),
                account_id: "synthetic-app".into(),
                credential_ref: "synthetic-key".into(),
                deadline: now() + 3600,
            };
            state.db(|db| {
                db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('c','owner','synthetic fallback',0)",[])?;
                for i in 1..=2 {
                    let run=format!("r{i}");let outbox=format!("b{i}");
                    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES(?1,'c','owner',?1,'synthetic','{}','completed',0,0)",[&run])?;
                    db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES(?1,?2,'synthetic-app','synthetic-key',?3,'synthetic','queued',?4,0)",params![outbox,run,serde_json::to_string(&target)?,target.deadline])?;
                }
                db.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status) VALUES('b1',2,'{}',0,'queued')",[])?;
                Ok(())
            }).unwrap();
            let item = state.db(|db| claim_in(db, now_ms())).unwrap().unwrap();
            assert_eq!(item.id, "b1");
            assert_eq!(item.seq, Some(2));
            let media = http_result(
                &base,
                "/upload",
                serde_json::json!({"file_type":2,"file_name":"synthetic.mp4"}),
                "预上传",
            )
            .await;
            let cause = media.as_ref().unwrap_err().1.clone();
            assert_eq!(cause, "QQ预上传返回HTTP 400，业务码40093001");
            let text_base = base.clone();
            let result = media_result(
                media,
                "video",
                "SYNTHETIC_PRIVATE_CAPTION",
                2,
                move |payload, seq| async move {
                    http_result(
                        &text_base,
                        "/messages",
                        serde_json::json!({"msg_seq":seq,"content":payload["text"]}),
                        "发送",
                    )
                    .await
                },
            )
            .await;
            let at = now_ms();
            match result {
                Ok((id, reason)) => {
                    assert_eq!(text_status, StatusCode::OK);
                    assert_eq!(reason.as_deref(), Some(cause.as_str()));
                    state
                        .db(|db| finish_sent_in(db, &item, &id, reason.as_deref(), at, 1500))
                        .unwrap();
                    let (parent,part)=state.db(|db|Ok(db.query_row("SELECT o.error,p.error FROM outbox o JOIN outbox_reply_parts p ON p.outbox_id=o.id WHERE o.id='b1'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?)).unwrap();
                    assert_eq!(parent, format!("媒体发送失败后已改发文字；原因：{cause}"));
                    assert_eq!(part, parent);
                    assert!(!part.contains("SYNTHETIC_PRIVATE_PLATFORM_BODY"));
                }
                Err((unknown, error)) => {
                    assert_ne!(text_status, StatusCode::OK);
                    assert_eq!(unknown, text_status == StatusCode::BAD_GATEWAY);
                    assert_eq!(
                        error,
                        format!("QQ发送返回HTTP {}，业务码未提供", text_status.as_u16())
                    );
                    state
                        .db(|db| {
                            finish_in(
                                db,
                                &item,
                                if unknown { "unknown" } else { "failed" },
                                None,
                                Some(&error),
                                at,
                                1500,
                            )
                        })
                        .unwrap();
                    if unknown {
                        assert!(state.db(|db| claim_in(db, at + 10000)).unwrap().is_none());
                    }
                }
            }
            let messages = mock.messages.lock().unwrap();
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].0, "upload");
            assert_eq!(messages[1].0, "text");
            assert_eq!(messages[1].1["msg_seq"], 2);
            assert_eq!(messages[1].1["content"], "视频发送暂不可用。");
            server.abort();
        }
    }

    #[tokio::test]
    async fn unknown_media_result_never_invokes_text_fallback() {
        let original = (true, "QQ网络中断，操作结果未知，未自动重试".to_owned());
        let invoked = std::sync::atomic::AtomicBool::new(false);
        let result = media_result(
            Err(original.clone()),
            "image",
            "synthetic",
            2,
            |_, _| async {
                invoked.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok("must-not-send".into())
            },
        )
        .await;
        assert_eq!(result.unwrap_err(), original);
        assert!(!invoked.load(std::sync::atomic::Ordering::SeqCst));
    }
    #[test]
    fn expired_predecessor_is_cleared_before_the_same_claim_selects_newer_reply() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: dir.path().into(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: dir.path().into(),
        })
        .unwrap();
        let target = ReplyTarget {
            kind: "c2c".into(),
            target: "user".into(),
            message_id: "inbound".into(),
            account_id: "app".into(),
            credential_ref: "key".into(),
            deadline: now() + 3600,
        };
        state.db(|db|{
            db.execute("INSERT INTO conversations VALUES('c','owner','fixture',NULL,0)",[])?;
            for i in 1..=2 {
                db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES(?1,'c','owner',?1,'q','{}','completed',0,0)",[format!("r{i}")])?;
                db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES(?1,?2,'app','key',?3,'reply','queued',?4,0)",params![format!("b{i}"),format!("r{i}"),serde_json::to_string(&target)?,if i==1 {0}else{target.deadline}])?;
            }
            Ok(())
        }).unwrap();
        let item = state.db(|db| claim_in(db, now_ms())).unwrap().unwrap();
        assert_eq!(item.id, "b2");
        assert_eq!(
            state
                .db(|db| Ok(db
                    .query_row("SELECT status FROM outbox WHERE id='b1'", [], |r| r
                        .get::<_, String>(0))?))
                .unwrap(),
            "expired"
        );
    }
    #[tokio::test]
    async fn missing_voice_asset_reaches_text_fallback_before_any_platform_request() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: dir.path().into(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: dir.path().into(),
        })
        .unwrap();
        let target = ReplyTarget {
            kind: "c2c".into(),
            target: "user".into(),
            message_id: "inbound".into(),
            account_id: "app".into(),
            credential_ref: "key".into(),
            deadline: now() + 3600,
        };
        let item=Delivery {id:"outbox".into(),seq:Some(1),target:target.clone(),content:serde_json::json!({"type":"voice","text":"保留可读文字。","delay_ms":0,"asset_id":uuid::Uuid::new_v4().to_string()}).to_string(),owner:"owner".into(),conversation:"c".into()};
        // QQ is deliberately disabled: its validation is a deterministic
        // transport sentinel, and no production account or request is used.
        let expected = crate::qq::send_qq_payload(
            &state,
            &target,
            &serde_json::json!({"type":"text","text":"（语音暂不可用）保留可读文字。"}),
            1,
        )
        .await
        .unwrap_err();
        assert_eq!(send(&state, &item).await.unwrap_err(), expected);
    }
    #[test]
    fn actual_send_completion_spaces_parts_and_unknown_fences_following_answers() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: dir.path().into(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: dir.path().into(),
        })
        .unwrap();
        let target = ReplyTarget {
            kind: "c2c".into(),
            target: "user".into(),
            message_id: "inbound".into(),
            account_id: "app".into(),
            credential_ref: "key".into(),
            deadline: now() + 3600,
        };
        state.db(|db|{
            db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('c','o','fixture',0)",[])?;
            for i in 1..=2{
                db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES(?1,'c','o',?1,'q','{}','completed',0,0)",[format!("r{i}")])?;
                db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES(?1,?2,'app','key',?3,'a','queued',?4,0)",params![format!("b{i}"),format!("r{i}"),serde_json::to_string(&target)?,target.deadline])?;
            }
            let p=crate::chat_reply::parse("一。二。",&Default::default()).unwrap();
            crate::chat_reply::queue_parts_in(db,"b1",&p)?;
            Ok(())
        }).unwrap();
        let first = state.db(|db| claim_in(db, 100)).unwrap().unwrap();
        assert_eq!(first.seq, Some(1));
        state
            .db(|db| finish_in(db, &first, "sent", Some("m1"), None, 5000, 1500))
            .unwrap();
        assert!(state.db(|db| claim_in(db, 6499)).unwrap().is_none());
        let second = state.db(|db| claim_in(db, 6500)).unwrap().unwrap();
        assert_eq!(second.seq, Some(2));
        state
            .db(|db| finish_in(db, &second, "unknown", None, Some("timeout"), 7000, 1500))
            .unwrap();
        assert!(state.db(|db| claim_in(db, 99999)).unwrap().is_none());
    }
}
