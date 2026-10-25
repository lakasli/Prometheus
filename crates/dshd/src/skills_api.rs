use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    skills::{self, Skill},
    state::{now, AppState},
};
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/skills", get(list).post(create))
        .route("/api/admin/v1/skills/images/{run_id}", get(image))
        .route(
            "/api/admin/v1/skills/{id}",
            axum::routing::patch(update).delete(remove),
        )
}
pub fn image_url(run_id: &str) -> String {
    format!("/api/admin/v1/skills/images/{run_id}")
}
async fn image(
    State(state): State<AppState>,
    admin: Admin,
    Path(run_id): Path<String>,
) -> ApiResult<Response> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let content=state.db(|db|Ok(db.query_row("SELECT response FROM runs WHERE id=?1 AND owner_id=?2 AND run_kind='skill_image' AND status='completed' AND length(response)<=6291456",params![run_id,admin.id],|r|r.get::<_,String>(0)).optional()?))?.ok_or_else(ApiError::not_found)?;
    let encoded = content
        .strip_prefix("![绘画](data:image/png;base64,")
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(ApiError::not_found)?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::not_found())?;
    if bytes.len() > 4 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(ApiError::not_found());
    }
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    name: String,
    description: String,
    instructions: String,
    kind: String,
    enabled: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    name: String,
    description: String,
    instructions: String,
    kind: String,
    enabled: bool,
    expected_revision: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    expected_revision: i64,
}
fn validate(body: &Body) -> ApiResult<()> {
    if body.name.trim().is_empty()
        || body.name.len() > 128
        || body.description.len() > 1024
        || body.instructions.trim().is_empty()
        || body.instructions.len() > 16000
        || !["instruction", "drawing"].contains(&body.kind.as_str())
        || body.name.chars().any(char::is_control)
        || body.description.contains('\0')
        || body.instructions.contains('\0')
    {
        return Err(ApiError::bad("invalid_skill_fields","名称须为1到128字节，介绍最多1024字节，指令须为1到16000字节；类型必须为 instruction 或 drawing"));
    }
    Ok(())
}
fn audit(db: &rusqlite::Connection, actor: &str, action: &str, id: &str) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,?2,?3,?4)",
        params![actor, action, id, now()],
    )?;
    Ok(())
}
async fn list(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Value>> {
    let (_, config) = state.current_config()?;
    Ok(Json(
        json!({"items":state.db(|db|skills::list(db))?,"drawing":{"native_available":config.model.image_generation && config.model.credential_ref.is_some(),"native_channels":["web"],"fallback":"html"}}),
    ))
}
async fn create(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<Body>,
) -> ApiResult<(StatusCode, Json<Skill>)> {
    validate(&body)?;
    let id = uuid::Uuid::new_v4().to_string();
    let item=state.db(|db| {
        let tx=db.transaction()?;
        let count:i64=tx.query_row("SELECT count(*) FROM skills",[],|r|r.get(0))?;
        if count>=64 {return Err(skills::SkillError{code:"skill_limit",message:"最多保存64个 Skill，请先删除不再需要的条目"}.into());}
        tx.execute("INSERT INTO skills(id,name,description,instructions,kind,enabled,revision,builtin,updated_at) VALUES(?1,?2,?3,?4,?5,?6,1,0,?7)",params![id,body.name.trim(),body.description,body.instructions,body.kind,body.enabled,now()])?;
        audit(&tx,&admin.id,"skill.create",&id)?;
        let item=skills::get(&tx,&id)?.expect("created skill");tx.commit()?;Ok(item)
    })?;
    Ok((StatusCode::CREATED, Json(item)))
}
async fn update(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Json(body): Json<Update>,
) -> ApiResult<Json<Skill>> {
    let revision = body.expected_revision;
    let body = Body {
        name: body.name,
        description: body.description,
        instructions: body.instructions,
        kind: body.kind,
        enabled: body.enabled,
    };
    validate(&body)?;
    let result=state.db(|db| {
        let tx=db.transaction()?;
        let Some(existing)=skills::get(&tx,&id)? else{return Ok(None)};
        if existing.revision!=revision{return Ok(Some(Err(())))};
        tx.execute("UPDATE skills SET name=?1,description=?2,instructions=?3,kind=?4,enabled=?5,revision=revision+1,updated_at=?6 WHERE id=?7",params![body.name.trim(),body.description,body.instructions,body.kind,body.enabled,now(),id])?;
        audit(&tx,&admin.id,"skill.update",&id)?;
        let item=skills::get(&tx,&id)?.expect("updated skill");tx.commit()?;Ok(Some(Ok(item)))
    })?;
    match result {
        None => Err(ApiError::not_found()),
        Some(Err(())) => Err(ApiError::conflict()),
        Some(Ok(item)) => Ok(Json(item)),
    }
}
async fn remove(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Json(body): Json<Expected>,
) -> ApiResult<Json<Value>> {
    let result = state.db(|db| {
        let tx = db.transaction()?;
        let Some(existing) = skills::get(&tx, &id)? else {
            return Ok(None);
        };
        if existing.revision != body.expected_revision {
            return Ok(Some(false));
        };
        tx.execute("DELETE FROM skills WHERE id=?1", [&id])?;
        audit(&tx, &admin.id, "skill.delete", &id)?;
        tx.commit()?;
        Ok(Some(true))
    })?;
    match result {
        None => Err(ApiError::not_found()),
        Some(false) => Err(ApiError::conflict()),
        Some(true) => Ok(Json(json!({"id":id,"deleted":true}))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    const ORIGIN: &str = "http://127.0.0.1:18419";
    async fn call(
        state: &AppState,
        method: &str,
        path: &str,
        body: Value,
        authenticated: bool,
        csrf: bool,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("/api/admin/v1{path}"))
            .header("Host", "127.0.0.1:18419")
            .header("Origin", ORIGIN)
            .header("Content-Type", "application/json");
        if authenticated {
            builder = builder.header("Cookie", format!("dsh_session={}", "a".repeat(64)));
        }
        if csrf {
            builder = builder.header("X-CSRF-Token", "skill-test-csrf");
        }
        let response = crate::api::router(state.clone())
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    #[tokio::test]
    async fn management_requires_auth_csrf_and_optimistic_revision_and_validates_content() {
        let temp = tempfile::TempDir::new().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: temp.path().to_owned(),
            origin: ORIGIN.into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: temp.path().join("web"),
        })
        .unwrap();
        state.db(|db|{db.execute("INSERT INTO admins(id,username,password_hash,created_at) VALUES('owner','admin','unused-fixture',0)",[])?;db.execute("INSERT INTO sessions(token_hash,user_id,csrf,expires_at) VALUES(?1,'owner','skill-test-csrf',?2)",params![crate::state::token_hash(&"a".repeat(64)),now()+3600])?;Ok(())}).unwrap();
        assert_eq!(
            call(&state, "GET", "/skills", Value::Null, false, false)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let body = json!({"name":"Test Skill","description":"original","instructions":"step one","kind":"instruction","enabled":true});
        assert_eq!(
            call(&state, "POST", "/skills", body.clone(), true, false)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let mut bad = body.clone();
        bad["instructions"] = json!("bad\u{0}instruction");
        assert_eq!(
            call(&state, "POST", "/skills", bad, true, true).await.0,
            StatusCode::BAD_REQUEST
        );
        let (status, item) = call(&state, "POST", "/skills", body.clone(), true, true).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(item["revision"], 1);
        assert_eq!(item["builtin"], false);
        let path = format!("/skills/{}", item["id"].as_str().unwrap());
        let mut updated = body.clone();
        updated["expected_revision"] = json!(1);
        updated["description"] = json!("updated introduction");
        updated["enabled"] = json!(false);
        let (status, item) = call(&state, "PATCH", &path, updated.clone(), true, true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(item["revision"], 2);
        assert_eq!(item["description"], "updated introduction");
        assert_eq!(item["enabled"], false);
        assert_eq!(
            call(&state, "PATCH", &path, updated, true, true).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(
                &state,
                "DELETE",
                &path,
                json!({"expected_revision":1}),
                true,
                true
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(
                &state,
                "DELETE",
                &path,
                json!({"expected_revision":2}),
                true,
                true
            )
            .await
            .0,
            StatusCode::OK
        );
        let (status, list) = call(&state, "GET", "/skills", Value::Null, true, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["items"].as_array().unwrap().len(), 2);
        assert_eq!(list["drawing"]["native_available"], false);
    }

    #[tokio::test]
    async fn native_images_are_owner_protected_and_history_responses_contain_only_references() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let temp = tempfile::TempDir::new().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: temp.path().to_owned(),
            origin: ORIGIN.into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: temp.path().join("web"),
        })
        .unwrap();
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[0, 40, 200])
                .unwrap();
        }
        let content = format!(
            "![绘画](data:image/png;base64,{})",
            STANDARD.encode(&png_bytes)
        );
        state.db(|db|{
            db.execute("INSERT INTO admins(id,username,password_hash,created_at) VALUES('owner','admin','unused-fixture',0)",[])?;
            db.execute("INSERT INTO sessions(token_hash,user_id,csrf,expires_at) VALUES(?1,'owner','skill-test-csrf',?2)",params![crate::state::token_hash(&"a".repeat(64)),now()+3600])?;
            db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('own','owner','test',0),('other','other-owner','test',0)",[])?;
            for(run,chat,owner) in [("image-own","own","owner"),("image-other","other","other-owner")] {
                db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at,run_kind) VALUES(?1,?2,?3,?1,'draw',?4,'completed',?5,0,0,'skill_image')",params![run,chat,owner,serde_json::to_string(&crate::config::RuntimeConfig::default())?,content])?;
                db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'assistant',?4,0)",params![format!("message-{run}"),chat,run,content])?;
            }
            Ok(())
        }).unwrap();
        assert_eq!(
            call(
                &state,
                "GET",
                "/skills/images/image-own",
                Value::Null,
                false,
                false
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                &state,
                "GET",
                "/skills/images/image-other",
                Value::Null,
                true,
                false
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        let response = crate::api::router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/admin/v1/skills/images/image-own")
                    .header("Host", "127.0.0.1:18419")
                    .header("Cookie", format!("dsh_session={}", "a".repeat(64)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert!(response.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-store"));
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            png_bytes.as_slice()
        );
        for path in ["/conversations/own/messages", "/runs", "/runs/image-own"] {
            let (status, body) = call(&state, "GET", path, Value::Null, true, false).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!body.to_string().contains("data:image"));
            if path == "/runs/image-own" {
                assert_eq!(
                    body["generated_image_url"],
                    "/api/admin/v1/skills/images/image-own"
                );
                assert_eq!(body["response"], "[原生生图结果]");
            }
            if path == "/conversations/own/messages" {
                assert_eq!(
                    body["items"][0]["generated_image_url"],
                    "/api/admin/v1/skills/images/image-own"
                );
            }
        }
    }
}
