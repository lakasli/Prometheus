//! Authenticated management only. These routes are not model tools; reading an
//! administrator-owned scope never grants access to a QQ/group/private owner.
use crate::{
    auth::{self, Admin},
    conversation_commitments::{
        self as commitments, ActivityBinding, Cancellation, ParticipantReview, Proposal,
        ProposalOrigin, Transition,
    },
    conversation_state::{self, VerifiedConversationScope},
    error::{ApiError, ApiResult},
    state::{now, AppState},
};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/commitments/scopes", get(scopes))
        .route(
            "/api/admin/v1/conversations/{conversation}/commitment-activities",
            get(activity_candidates),
        )
        .route(
            "/api/admin/v1/conversations/{conversation}/commitments",
            get(list).post(propose),
        )
        .route(
            "/api/admin/v1/conversations/{conversation}/commitments/{id}",
            get(detail),
        )
        .route(
            "/api/admin/v1/conversations/{conversation}/commitments/{id}/confirm",
            post(confirm),
        )
        .route(
            "/api/admin/v1/conversations/{conversation}/commitments/{id}/participants",
            post(review_participants),
        )
        .route(
            "/api/admin/v1/conversations/{conversation}/commitments/{id}/activity",
            post(bind),
        )
        .route(
            "/api/admin/v1/conversations/{conversation}/commitments/{id}/cancel",
            post(cancel),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeQuery {
    run_id: String,
    after: Option<i64>,
    limit: Option<usize>,
}

fn access(state: &AppState, headers: &HeaderMap, admin: &Admin, write: bool) -> ApiResult<()> {
    let expected = url::Url::parse(&state.0.options.origin).map_err(|_| ApiError::internal())?;
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("缺少Host"))?;
    let actual = url::Url::parse(&format!("{}://{host}", expected.scheme()))
        .map_err(|_| ApiError::forbidden("Host无效"))?;
    if actual.host_str() != expected.host_str()
        || actual.port_or_known_default() != expected.port_or_known_default()
    {
        return Err(ApiError::forbidden("Host不在管理来源内"));
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        if origin.to_str().ok() != Some(state.0.options.origin.as_str()) {
            return Err(ApiError::forbidden("Origin不在管理来源内"));
        }
    } else if write {
        return Err(ApiError::forbidden("写操作必须携带Origin"));
    }
    if write
        && !auth::secure_eq(
            headers
                .get("x-csrf-token")
                .and_then(|h| h.to_str().ok())
                .unwrap_or(""),
            &admin.csrf_token,
        )
    {
        return Err(ApiError::forbidden("CSRF校验失败"));
    }
    Ok(())
}

fn expected(headers: &HeaderMap) -> ApiResult<i64> {
    let text = headers
        .get(header::IF_MATCH)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "需要精确If-Match版本",
            )
        })?;
    text.strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| ApiError::bad("invalid_if_match", "If-Match须为带引号的当前正整数版本"))
}

fn key(headers: &HeaderMap) -> ApiResult<&str> {
    headers
        .get("idempotency-key")
        .and_then(|h| h.to_str().ok())
        .filter(|k| !k.is_empty() && k.len() <= 128 && !k.chars().any(char::is_control))
        .ok_or_else(|| ApiError::bad("idempotency_key_required", "写操作需要Idempotency-Key"))
}

fn scoped(
    db: &Connection,
    admin: &Admin,
    conversation: &str,
    run: &str,
) -> anyhow::Result<VerifiedConversationScope> {
    let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM runs r JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id WHERE r.id=?1 AND c.id=?2)",rusqlite::params![run,conversation],|r|r.get(0))?;
    if !exists {
        return Err(crate::trigger_store::missing());
    }
    let scope = conversation_state::management_scope_for_source_in(db, admin, run)?;
    if commitments::management_access_in(db, &scope, admin).is_err()
        || scope.conversation_id() != conversation
    {
        return Err(crate::trigger_store::missing());
    }
    Ok(scope)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopePage {
    after: Option<i64>,
    limit: Option<usize>,
}

async fn scopes(
    State(state): State<AppState>,
    admin: Admin,
    Query(page): Query<ScopePage>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    access(&state, &headers, &admin, false)?;
    let limit = page.limit.unwrap_or(20);
    if !(1..=50).contains(&limit) || page.after.is_some_and(|v| v <= 0) {
        return Err(ApiError::bad(
            "invalid_scope_page",
            "来源分页大小须为1到50，游标须为正整数",
        ));
    }
    Ok(Json(state.db(|db|{
        let tx=db.transaction()?;
        // Group identity comes exclusively from authenticated platform evidence;
        // these raw keys stay in SQL and never enter the response.
        let mut q=tx.prepare("WITH eligible AS(
          SELECT r.rowid AS source_order,r.id,r.conversation_id,r.owner_id,r.created_at,c.title,b.binding_json,
          CASE WHEN json_extract(b.binding_json,'$.channel')='qq' AND json_extract(b.binding_json,'$.conversation_kind') IN('group','channel','dms')
            THEN (SELECT CASE WHEN count(*)>0 AND count(*)=count(json_extract(i.identity_json,'$.sender_id'))
              AND count(DISTINCT json_array(json_extract(i.identity_json,'$.namespace'),json_extract(i.identity_json,'$.sender_id')))=1
              THEN min(json_array(json_extract(i.identity_json,'$.namespace'),json_extract(i.identity_json,'$.sender_id'))) END
              FROM conversation_message_identities i WHERE i.run_id=r.id)
            ELSE 'private' END AS actor_key
          FROM runs r JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id
          JOIN run_conversation_bindings b ON b.run_id=r.id
          WHERE r.run_kind IN('model','skill_text') AND coalesce(json_extract(r.character_snapshot_json,'$.is_test'),0)=0
          AND (r.owner_id=?1 OR ((r.owner_id LIKE 'qq:%' OR r.owner_id LIKE 'weixin:%') AND NOT EXISTS(SELECT 1 FROM admins a WHERE a.id=r.owner_id))))
          SELECT source_order,id,conversation_id,created_at,title,binding_json FROM eligible
          WHERE actor_key IS NOT NULL AND source_order IN(SELECT max(source_order) FROM eligible WHERE actor_key IS NOT NULL GROUP BY conversation_id,actor_key)
          AND (?2 IS NULL OR source_order<?2) ORDER BY source_order DESC LIMIT 200")?;
        let candidates=q.query_map(rusqlite::params![admin.id,page.after],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut items=Vec::new();let mut cursor=None;let candidate_count=candidates.len();
        for(order,run,conversation,at,title,text)in candidates{
            cursor=Some(order);let scope=match conversation_state::management_scope_for_source_in(&tx,&admin,&run){Ok(scope)=>scope,Err(_)=>continue};
            if scope.speaker_scope().is_none()||commitments::management_access_in(&tx,&scope,&admin).is_err(){continue;}
            let source=conversation_state::run_input_source_in(&tx,&scope)?;if !conversation_state::validate_source_in(&tx,&scope,&source)?{continue;}
            let binding:Value=serde_json::from_str(&text)?;let channel=binding["channel"].as_str().unwrap_or("unknown");let kind=binding["conversation_kind"].as_str().unwrap_or("unknown");
            let label=match(channel,kind){("qq","c2c")=>"QQ 私聊",("qq","group")=>"QQ 群会话",("qq","channel")=>"QQ 频道会话",("qq","dms")=>"QQ 频道私聊",("weixin",_)=>"微信私聊",("web_or_cli",_)=>"网页或命令行",_=>"未命名来源"};
            let title=if channel=="web_or_cli"&&!title.trim().is_empty(){title.chars().filter(|c|!c.is_control()).take(120).collect::<String>()}else{"未命名来源".into()};
            items.push(serde_json::json!({"conversation_id":conversation,"run_id":run,"source_run_id":run,"title":title,"source_label":label,"channel":channel,"source_kind":kind,"scope_ref":scope.key(),"updated_at":at}));
            if items.len()==limit{break;}
        }
        let next=if items.len()==limit||candidate_count==200{cursor}else{None};drop(q);tx.commit()?;
        Ok(serde_json::json!({"items":items,"next_cursor":next,"limit":limit,"identities":"verified_source_scope_only"}))
    })?))
}

async fn activity_candidates(
    State(state): State<AppState>,
    admin: Admin,
    Path(conversation): Path<String>,
    Query(query): Query<ScopeQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    access(&state, &headers, &admin, false)?;
    Ok(Json(state.db(|db| {
        let tx = db.transaction()?;
        let scope = scoped(&tx, &admin, &conversation, &query.run_id)?;
        let value = commitments::activity_candidates_in(
            &tx,
            &scope,
            &admin,
            query.after.unwrap_or(0),
            query.limit.unwrap_or(20),
            now(),
        )?;
        tx.commit()?;
        Ok(value)
    })?))
}

fn tagged(value: Value) -> Response {
    (
        [(
            header::ETAG,
            format!("\"{}\"", value["revision"].as_i64().unwrap_or(0)),
        )],
        Json(value),
    )
        .into_response()
}

async fn list(
    State(state): State<AppState>,
    admin: Admin,
    Path(conversation): Path<String>,
    Query(query): Query<ScopeQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    access(&state, &headers, &admin, false)?;
    Ok(Json(state.db(|db| {
        let tx = db.transaction()?;
        let scope = scoped(&tx, &admin, &conversation, &query.run_id)?;
        let value = commitments::list_in(
            &tx,
            &scope,
            query.after.unwrap_or(0),
            query.limit.unwrap_or(20),
            now(),
        )?;
        tx.commit()?;
        Ok(value)
    })?))
}

async fn detail(
    State(state): State<AppState>,
    admin: Admin,
    Path((conversation, id)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, false)?;
    let value = state.db(|db| {
        let tx = db.transaction()?;
        let scope = scoped(&tx, &admin, &conversation, &query.run_id)?;
        let value = commitments::detail_in(&tx, &scope, &id, now())?;
        tx.commit()?;
        Ok(value)
    })?;
    Ok(tagged(value))
}

async fn propose(
    State(state): State<AppState>,
    admin: Admin,
    Path(conversation): Path<String>,
    Query(query): Query<ScopeQuery>,
    headers: HeaderMap,
    Json(input): Json<Proposal>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, true)?;
    let key = key(&headers)?;
    let value = state.db(|db| {
        let tx = db.transaction()?;
        let scope = scoped(&tx, &admin, &conversation, &query.run_id)?;
        let value = commitments::propose_in(
            &tx,
            &scope,
            &input,
            ProposalOrigin::ManagementCandidate,
            key,
            now(),
        )?;
        tx.commit()?;
        Ok(value)
    })?;
    Ok(tagged(value))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    confirmed: bool,
}

async fn confirm(
    State(state): State<AppState>,
    admin: Admin,
    Path((conversation, id)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
    headers: HeaderMap,
    Json(input): Json<Confirmation>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, true)?;
    let rev = expected(&headers)?;
    let key = key(&headers)?;
    if !input.confirmed {
        return Err(ApiError::bad(
            "explicit_confirmation_required",
            "确认约定须明确confirmed=true",
        ));
    }
    let value = state.db(|db| {
        let tx = db.transaction()?;
        let scope = scoped(&tx, &admin, &conversation, &query.run_id)?;
        let change = Transition {
            expected_revision: rev,
            idempotency_key: key,
            now: now(),
        };
        let value = commitments::confirm_in(&tx, &scope, &admin, &id, &change)?;
        tx.commit()?;
        Ok(value)
    })?;
    Ok(tagged(value))
}

async fn bind(
    State(state): State<AppState>,
    admin: Admin,
    Path((conversation, id)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
    headers: HeaderMap,
    Json(input): Json<ActivityBinding>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, true)?;
    let rev = expected(&headers)?;
    let key = key(&headers)?;
    let value = state.db(|db| {
        let tx = db.transaction()?;
        let scope = scoped(&tx, &admin, &conversation, &query.run_id)?;
        let change = Transition {
            expected_revision: rev,
            idempotency_key: key,
            now: now(),
        };
        let value = commitments::bind_activity_in(&tx, &scope, &admin, &id, &input, &change)?;
        tx.commit()?;
        Ok(value)
    })?;
    Ok(tagged(value))
}

async fn review_participants(
    State(state): State<AppState>,
    admin: Admin,
    Path((conversation, id)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
    headers: HeaderMap,
    Json(input): Json<ParticipantReview>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, true)?;
    let rev = expected(&headers)?;
    let key = key(&headers)?;
    let value = state.db(|db| {
        let tx = db.transaction()?;
        let scope = scoped(&tx, &admin, &conversation, &query.run_id)?;
        let change = Transition {
            expected_revision: rev,
            idempotency_key: key,
            now: now(),
        };
        let value = commitments::review_participants_in(&tx, &scope, &admin, &id, &input, &change)?;
        tx.commit()?;
        Ok(value)
    })?;
    Ok(tagged(value))
}

async fn cancel(
    State(state): State<AppState>,
    admin: Admin,
    Path((conversation, id)): Path<(String, String)>,
    Query(query): Query<ScopeQuery>,
    headers: HeaderMap,
    Json(input): Json<Cancellation>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, true)?;
    let rev = expected(&headers)?;
    let key = key(&headers)?;
    let value = state.db(|db| {
        let tx = db.transaction()?;
        let scope = scoped(&tx, &admin, &conversation, &query.run_id)?;
        let change = Transition {
            expected_revision: rev,
            idempotency_key: key,
            now: now(),
        };
        let value = commitments::cancel_in(&tx, &scope, &admin, &id, &input, &change)?;
        tx.commit()?;
        Ok(value)
    })?;
    Ok(tagged(value))
}
