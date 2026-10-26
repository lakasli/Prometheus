//! Authenticated management routes; merge before the shared Origin/CSRF guard.
use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    relationships::{self, Access},
    state::{now, AppState},
};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/admin/v1/relationships/capabilities",
            get(capabilities),
        )
        .route(
            "/api/admin/v1/relationships/affinity-policy",
            get(get_affinity_policy).put(save_affinity_policy),
        )
        .route(
            "/api/admin/v1/relationships/affinity-policy/preview",
            post(preview_affinity_policy),
        )
        .route("/api/admin/v1/relationships/people", get(list).post(create))
        .route(
            "/api/admin/v1/relationships/people/{id}",
            get(detail).patch(rename).delete(forget),
        )
        .route(
            "/api/admin/v1/relationships/people/{id}/assertions",
            post(propose),
        )
        .route(
            "/api/admin/v1/relationships/assertions/{id}/review",
            post(review),
        )
        .route(
            "/api/admin/v1/relationships/assertions/{id}",
            axum::routing::delete(forget_assertion),
        )
        .route(
            "/api/admin/v1/relationships/people/{id}/identities",
            post(bind),
        )
        .route(
            "/api/admin/v1/relationships/people/{id}/identities/{identity_id}",
            axum::routing::delete(unbind),
        )
        .route("/api/admin/v1/relationships/people/{id}/merge", post(merge))
        .route("/api/admin/v1/relationships/people/{id}/split", post(split))
        .route(
            "/api/admin/v1/relationships/people/{id}/identity-group",
            post(link_identity_group).delete(unlink_identity_group),
        )
        .route(
            "/api/admin/v1/relationships/edges",
            get(edges).post(create_edge),
        )
        .route(
            "/api/admin/v1/relationships/model-jobs",
            get(model_jobs).post(enqueue_model),
        )
        .route(
            "/api/admin/v1/relationships/model-jobs/{id}",
            get(model_job),
        )
        .route(
            "/api/admin/v1/relationships/edges/{id}/review",
            post(review_edge),
        )
        .route(
            "/api/admin/v1/relationships/edges/{id}",
            axum::routing::delete(forget_edge),
        )
        .route(
            "/api/admin/v1/relationships/people/{id}/affinity/events",
            get(affinity_events).post(add_affinity),
        )
        .route(
            "/api/admin/v1/relationships/people/{id}/contact-permission",
            axum::routing::put(contact),
        )
        .layer(DefaultBodyLimit::max(32 * 1024))
}
fn tagged<T: serde::Serialize>(revision: i64, item: T) -> Response {
    ([(header::ETAG, format!("\"{revision}\""))], Json(item)).into_response()
}
fn expected(h: &HeaderMap) -> ApiResult<i64> {
    let raw = h
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "请提供当前版本If-Match",
            )
        })?;
    let s = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    s.parse::<i64>()
        .ok()
        .filter(|x| *x >= 0)
        .ok_or_else(|| ApiError::bad("invalid_if_match", "If-Match须为当前非负整数版本"))
}
async fn capabilities(_admin: Admin) -> Json<Value> {
    Json(
        json!({"people":true,"field_provenance":true,"review_required":true,"directed_edges":true,"identity_mapping":"explicit_confirmation","merge":"same_source_scope_only","affinity":{"initial":50,"min":0,"max":100,"daily_decay":1,"grace_seconds":259200,"event_limit":5,"daily_limit":10,"policy_revision":1},"proactive_delivery":false,"observed_friend_import":false,"automatic_profile_candidates":false,"blocked_reasons":["好友观察须另行授权与适配","主动消息须接收方许可及真实渠道独立验收"]}),
    )
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelQuery {
    person_id: Option<String>,
}
async fn get_affinity_policy(
    State(s): State<AppState>,
    _a: Admin,
) -> ApiResult<Json<relationships::affinity_policy::Policy>> {
    Ok(Json(s.db(|db| {
        Ok(relationships::affinity_policy::current(db))
    })??))
}
async fn save_affinity_policy(
    State(s): State<AppState>,
    a: Admin,
    h: HeaderMap,
    Json(b): Json<relationships::affinity_policy::PolicyInput>,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::affinity_policy::save(
            db,
            Access::Admin(&a.id),
            rev,
            &b,
        ))
    })??;
    Ok(tagged(p.revision, p))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyPreview {
    policy: relationships::affinity_policy::PolicyInput,
    idle_seconds: i64,
}
async fn preview_affinity_policy(
    State(s): State<AppState>,
    _a: Admin,
    Json(b): Json<PolicyPreview>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.db(|db| {
        Ok(relationships::affinity_policy::preview(
            db,
            &b.policy,
            b.idle_seconds,
        ))
    })??))
}
async fn model_jobs(
    State(s): State<AppState>,
    _a: Admin,
    Query(q): Query<ModelQuery>,
) -> ApiResult<Json<Value>> {
    let items = s.db(|db| Ok(crate::relationships_model::list(db, q.person_id.as_deref())))??;
    Ok(Json(json!({"items":items})))
}
async fn model_job(
    State(s): State<AppState>,
    _a: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<crate::relationships_model::Job>> {
    Ok(Json(
        s.db(|db| Ok(crate::relationships_model::get(db, &id)))??,
    ))
}
async fn enqueue_model(
    State(s): State<AppState>,
    a: Admin,
    h: HeaderMap,
    Json(b): Json<crate::relationships_model::GenerateInput>,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let job = s.db(|db| Ok(crate::relationships_model::enqueue(db, &a.id, rev, &b)))??;
    s.0.notify.notify_waiters();
    Ok((StatusCode::ACCEPTED, Json(job)).into_response())
}
async fn list(
    State(s): State<AppState>,
    a: Admin,
    Query(q): Query<relationships::ListOptions>,
) -> ApiResult<Json<relationships::PeoplePage>> {
    Ok(Json(s.db(|db| {
        Ok(relationships::list(db, Access::Admin(&a.id), &q))
    })??))
}
async fn create(
    State(s): State<AppState>,
    a: Admin,
    Json(b): Json<relationships::CreatePerson>,
) -> ApiResult<Response> {
    let p = s.db(|db| Ok(relationships::create(db, Access::Admin(&a.id), &b)))??;
    Ok((StatusCode::CREATED, tagged(p.revision, p)).into_response())
}
async fn detail(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let p = s.db(|db| Ok(relationships::detail(db, Access::Admin(&a.id), &id, now())))??;
    Ok(tagged(p.person.revision, p))
}
async fn rename(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::RenamePerson>,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::rename(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
            &b,
        ))
    })??;
    Ok(tagged(p.revision, p))
}
async fn forget(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::forget_person(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
        ))
    })??;
    Ok(tagged(rev + 1, p))
}
async fn propose(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::CreateAssertion>,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::propose_assertion(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
            &b,
        ))
    })??;
    Ok((StatusCode::CREATED, tagged(p.revision, p)).into_response())
}
async fn review(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::ReviewAssertion>,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::review_assertion(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
            &b,
        ))
    })??;
    Ok(tagged(p.revision, p))
}
async fn forget_assertion(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::forget_assertion(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
        ))
    })??;
    Ok(tagged(rev + 1, p))
}
async fn bind(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::BindIdentity>,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::bind_identity(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
            &b,
        ))
    })??;
    Ok(tagged(p.person.revision, p))
}
async fn unbind(
    State(s): State<AppState>,
    a: Admin,
    Path((id, iid)): Path<(String, String)>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::unbind_identity(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
            &iid,
        ))
    })??;
    Ok(tagged(rev + 1, p))
}
async fn merge(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::MergePeople>,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| Ok(relationships::merge(db, Access::Admin(&a.id), &id, rev, &b)))??;
    Ok(tagged(rev + 1, p))
}
async fn split(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::ConfirmChange>,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| Ok(relationships::split(db, Access::Admin(&a.id), &id, rev, &b)))??;
    Ok(tagged(p.revision, p))
}
async fn link_identity_group(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::identity_groups::LinkInput>,
) -> ApiResult<Json<relationships::identity_groups::IdentityGroup>> {
    let rev = expected(&h)?;
    Ok(Json(s.db(|db| {
        Ok(relationships::identity_groups::link(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
            &b,
        ))
    })??))
}
async fn unlink_identity_group(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::ConfirmChange>,
) -> ApiResult<Json<Value>> {
    let rev = expected(&h)?;
    Ok(Json(s.db(|db| {
        Ok(relationships::identity_groups::unlink(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
            &b,
        ))
    })??))
}
async fn edges(State(s): State<AppState>, a: Admin) -> ApiResult<Json<Value>> {
    let items = s.db(|db| Ok(relationships::edges(db, Access::Admin(&a.id))))??;
    Ok(Json(json!({"items":items})))
}
async fn create_edge(
    State(s): State<AppState>,
    a: Admin,
    Json(b): Json<relationships::CreateEdge>,
) -> ApiResult<Response> {
    let p = s.db(|db| Ok(relationships::create_edge(db, Access::Admin(&a.id), &b)))??;
    Ok((StatusCode::CREATED, tagged(p.revision, p)).into_response())
}
async fn review_edge(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::ReviewAssertion>,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::review_edge(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
            &b,
        ))
    })??;
    Ok(tagged(p.revision, p))
}
async fn forget_edge(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::forget_edge(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
        ))
    })??;
    Ok(tagged(rev + 1, p))
}
async fn affinity_events(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = s.db(|db| {
        Ok(relationships::affinity_events(
            db,
            Access::Admin(&a.id),
            &id,
        ))
    })??;
    Ok(Json(json!({"items":p})))
}
async fn add_affinity(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::AffinityInput>,
) -> ApiResult<Json<relationships::Affinity>> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::add_affinity(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
            &b,
        ))
    })??;
    Ok(Json(p))
}
async fn contact(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(b): Json<relationships::ContactInput>,
) -> ApiResult<Response> {
    let rev = expected(&h)?;
    let p = s.db(|db| {
        Ok(relationships::set_contact(
            db,
            Access::Admin(&a.id),
            &id,
            rev,
            &b,
        ))
    })??;
    Ok(tagged(p.revision, p))
}
