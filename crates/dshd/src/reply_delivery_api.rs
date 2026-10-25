//! An operator can abandon unsent parts after checking an uncertain QQ receipt.
//! This never resends a request or claims that the uncertain message arrived.
use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    state::{now, AppState},
};
use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/qq/deliveries", get(list))
        .route(
            "/api/admin/v1/qq/deliveries/{id}/resolve",
            axum::routing::post(resolve),
        )
}
async fn list(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Value>> {
    let (_, config) = state.current_config()?;
    let items=state.db(|db|{
        let mut q=db.prepare("SELECT id,run_id,status,error,created_at,(SELECT count(*) FROM outbox_reply_parts p WHERE p.outbox_id=o.id AND p.status='queued') FROM outbox o WHERE account_id=?1 AND credential_ref=?2 AND status IN ('unknown','resolved_unknown') ORDER BY rowid DESC LIMIT 50")?;
        let rows=q.query_map(params![config.qq.app_id,config.qq.credential_ref],|r|Ok(json!({"id":r.get::<_,String>(0)?,"run_id":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"error":r.get::<_,Option<String>>(3)?,"created_at":r.get::<_,i64>(4)?,"unsent_parts":r.get::<_,i64>(5)?})))?;
        Ok(rows.collect::<Result<Vec<_>,_>>()?)
    })?;
    Ok(Json(json!({"items":items,"retries_automatically":false})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolve {
    expected_status: String,
    abandon_remaining: bool,
    checked_client: bool,
}
async fn resolve(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Json(body): Json<Resolve>,
) -> ApiResult<Json<Value>> {
    if body.expected_status != "unknown" || !body.abandon_remaining || !body.checked_client {
        return Err(ApiError::bad(
            "delivery_review_required",
            "请先核对QQ客户端，并明确仅放弃旧回复的未发部分",
        ));
    }
    let (_, config) = state.current_config()?;
    let changed=state.db(|db|{
        let tx=db.transaction()?;
        let n=tx.execute("UPDATE outbox SET status='resolved_unknown' WHERE id=?1 AND status='unknown' AND account_id=?2 AND credential_ref=?3",params![id,config.qq.app_id,config.qq.credential_ref])?;
        if n!=1{return Ok(false);}
        tx.execute("UPDATE outbox_reply_parts SET status='cancelled',error='管理员核对后放弃未发部分，未重发不确定消息' WHERE outbox_id=?1 AND status='queued'",[&id])?;
        tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'qq.delivery_abandon_remaining',?2,?3)",params![admin.id,id,now()])?;
        tx.commit()?;Ok(true)
    })?;
    if !changed {
        return Err(ApiError::conflict());
    }
    state.0.notify.notify_waiters();
    Ok(Json(
        json!({"status":"resolved_unknown","delivery_confirmed":false,"resent":false,"remaining_cancelled":true}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn review_abandons_only_unsent_parts_and_never_confirms_or_retries_the_unknown_request() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: dir.path().into(),
            origin: "http://localhost".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: dir.path().into(),
        })
        .unwrap();
        state.db(|db|{
            let(_,mut cfg)=crate::character::current_config_in(db)?;cfg.qq.app_id="123".into();cfg.qq.credential_ref=Some("key".into());
            db.execute("UPDATE config_revisions SET config_json=?1 WHERE revision=1",[serde_json::to_string(&cfg)?])?;
            db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('c','qq:owner','test',0)",[])?;
            db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES('r','c','qq:owner','m','q','{}','completed',0,0)",[])?;
            db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES('b','r','123','key','{}','a','unknown',1,0)",[])?;
            for(seq,status)in[(1,"unknown"),(2,"queued")] {db.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status) VALUES('b',?1,'{}',1500,?2)",params![seq,status])?;}
            Ok(())
        }).unwrap();
        let admin = Admin {
            id: "admin".into(),
            username: "test".into(),
            role: "admin".into(),
            csrf_token: "test".into(),
        };
        assert!(resolve(
            State(state.clone()),
            admin.clone(),
            Path("b".into()),
            Json(Resolve {
                expected_status: "unknown".into(),
                abandon_remaining: true,
                checked_client: false
            })
        )
        .await
        .is_err());
        let result = resolve(
            State(state.clone()),
            admin.clone(),
            Path("b".into()),
            Json(Resolve {
                expected_status: "unknown".into(),
                abandon_remaining: true,
                checked_client: true,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(result["delivery_confirmed"], false);
        assert_eq!(result["resent"], false);
        state
            .db(|db| {
                let mut q = db.prepare("SELECT status FROM outbox_reply_parts ORDER BY seq")?;
                let s = q
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                assert_eq!(s, vec!["unknown", "cancelled"]);
                Ok(())
            })
            .unwrap();
        assert!(resolve(
            State(state.clone()),
            admin,
            Path("b".into()),
            Json(Resolve {
                expected_status: "unknown".into(),
                abandon_remaining: true,
                checked_client: true
            })
        )
        .await
        .is_err());
    }
}
