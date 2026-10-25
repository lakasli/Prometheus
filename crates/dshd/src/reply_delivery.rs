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
    // The oldest unfinished message in one conversation fences newer answers.
    // Another conversation may continue while this one waits for its interval.
    let item=tx.query_row("SELECT o.id,o.target_json,o.content,r.owner_id,r.conversation_id,p.seq,p.payload_json FROM outbox o JOIN runs r ON r.id=o.run_id LEFT JOIN outbox_reply_parts p ON p.outbox_id=o.id AND p.seq=(SELECT min(q.seq) FROM outbox_reply_parts q WHERE q.outbox_id=o.id AND q.status='queued') WHERE o.status='queued' AND o.deadline>=?1 AND (p.seq IS NULL OR p.due_ms<=?2) AND NOT EXISTS(SELECT 1 FROM outbox_reply_parts u WHERE u.outbox_id=o.id AND u.status IN ('unknown','failed','sending')) AND NOT EXISTS(SELECT 1 FROM reply_account_pacing a WHERE a.account_id=o.account_id AND a.next_ms>?2) AND NOT EXISTS(SELECT 1 FROM outbox older JOIN runs prior ON prior.id=older.run_id WHERE prior.conversation_id=r.conversation_id AND older.rowid<o.rowid AND older.status IN ('queued','sending','unknown')) ORDER BY o.rowid LIMIT 1",params![now(),at],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<i64>>(5)?,r.get::<_,Option<String>>(6)?))).optional()?;
    let Some((id, target, content, owner, conversation, seq, payload)) = item else {
        tx.execute("UPDATE outbox SET status='expired',error='回复截止时间已过，未发送' WHERE status='queued' AND deadline<?1",[now()])?;
        tx.commit()?;
        return Ok(None);
    };
    let target: ReplyTarget = serde_json::from_str(&target)?;
    tx.execute("UPDATE outbox SET status='sending' WHERE id=?1", [&id])?;
    if let Some(seq) = seq {
        tx.execute(
            "UPDATE outbox_reply_parts SET status='sending' WHERE outbox_id=?1 AND seq=?2",
            params![id, seq],
        )?;
    }
    tx.commit()?;
    Ok(Some(Delivery {
        id,
        seq,
        target,
        content: payload.unwrap_or(content),
        owner,
        conversation,
    }))
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

async fn send(state: &AppState, item: &Delivery) -> Result<(String, bool), (bool, String)> {
    let Some(seq) = item.seq else {
        return crate::qq::send_qq_payload(
            state,
            &item.target,
            &serde_json::json!({"type":"text","text":item.content}),
            1,
        )
        .await
        .map(|id| (id, false));
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
        .map(|id| (id, false));
    }
    let id = part
        .asset_id
        .as_ref()
        .ok_or((false, "媒体制品尚未准备".into()))?;
    let bytes = if part.kind == "voice" {
        let (meta, bytes) = crate::speech::read_asset(state, &item.owner, id)
            .map_err(|_| (false, "语音制品已失效".into()))?;
        if meta.conversation_id.as_deref() != Some(&item.conversation) {
            return Err((false, "语音制品会话不匹配".into()));
        }
        bytes
    } else {
        let (meta, bytes) = crate::media_assets::read(state, &item.owner, &item.conversation, id)
            .map_err(|_| (false, "媒体制品不可用或归属不匹配".into()))?;
        if meta.kind != part.kind {
            return Err((false, "媒体类型不匹配".into()));
        }
        bytes
    };
    match crate::qq::send_media_bytes(
        state,
        &item.target,
        &part.kind,
        &bytes,
        &part.text,
        seq as u32,
    )
    .await
    {
        Ok(id) => Ok((id, false)),
        Err((false, _)) => {
            let text = if part.kind == "voice" {
                format!("（语音暂不可用）{}", part.text)
            } else {
                format!(
                    "{}发送暂不可用。",
                    if part.kind == "image" {
                        "图片"
                    } else {
                        "视频"
                    }
                )
            };
            crate::qq::send_qq_payload(
                state,
                &item.target,
                &serde_json::json!({"type":"text","text":text}),
                seq as u32,
            )
            .await
            .map(|id| (id, true))
        }
        Err(error) => Err(error),
    }
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
            Ok((id, fallback)) => state.db(|db| {
                finish_in(
                    db,
                    &item,
                    "sent",
                    Some(&id),
                    if fallback {
                        Some("媒体发送失败后已改发文字")
                    } else {
                        None
                    },
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
