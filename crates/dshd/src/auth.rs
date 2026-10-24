use crate::{
    error::{ApiError, ApiResult},
    state::{now, random_token, token_hash, AppState},
};
use argon2::{
    password_hash::{PasswordHasher, PasswordVerifier, SaltString},
    Argon2, PasswordHash,
};
use axum::{
    extract::{FromRequestParts, Request, State},
    http::{header, request::Parts, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use rand::rngs::OsRng;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

#[derive(Clone, Serialize)]
pub struct Admin {
    pub id: String,
    pub username: String,
    pub role: String,
    pub csrf_token: String,
}

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        authenticate(state, &parts.headers)
    }
}

pub fn authenticate(state: &AppState, headers: &HeaderMap) -> ApiResult<Admin> {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let token = cookie
        .split(';')
        .find_map(|p| p.trim().strip_prefix("dsh_session="))
        .filter(|v| v.len() == 64)
        .ok_or_else(ApiError::unauthorized)?;
    let found=state.db(|db| Ok(db.query_row("SELECT a.id,a.username,s.csrf FROM sessions s JOIN admins a ON a.id=s.user_id WHERE s.token_hash=?1 AND s.expires_at>?2",params![token_hash(token),now()],|r|Ok(Admin{id:r.get(0)?,username:r.get(1)?,role:"admin".into(),csrf_token:r.get(2)?})).optional()?))?;
    found.ok_or_else(ApiError::unauthorized)
}

pub async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path.starts_with("/api/admin/") {
        if let Err(error) = check_origin(&state, request.headers(), request.method().is_safe()) {
            return error.into_response();
        }
        if !request.method().is_safe()
            && !matches!(
                path,
                "/api/admin/v1/auth/login" | "/api/admin/v1/auth/bootstrap"
            )
        {
            let admin = match authenticate(&state, request.headers()) {
                Ok(a) => a,
                Err(e) => return e.into_response(),
            };
            let csrf = request
                .headers()
                .get("x-csrf-token")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
            if !secure_eq(csrf, &admin.csrf_token) {
                return ApiError::forbidden("CSRF校验失败，请重新登录").into_response();
            }
        }
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
        .headers_mut()
        .insert("x-frame-options", "DENY".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "same-origin".parse().unwrap());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response.headers_mut().insert("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'".parse().unwrap());
    response
}

fn check_origin(state: &AppState, headers: &HeaderMap, safe: bool) -> ApiResult<()> {
    let expected = url::Url::parse(&state.0.options.origin).map_err(|_| ApiError::internal())?;
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("缺少Host"))?;
    let actual = url::Url::parse(&format!("{}://{}", expected.scheme(), host))
        .map_err(|_| ApiError::forbidden("Host无效"))?;
    if actual.host_str() != expected.host_str()
        || actual.port_or_known_default() != expected.port_or_known_default()
    {
        return Err(ApiError::forbidden("Host不在配置的管理来源内"));
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        if origin.to_str().ok() != Some(state.0.options.origin.as_str()) {
            return Err(ApiError::forbidden("Origin不在配置的管理来源内"));
        }
    } else if !safe {
        return Err(ApiError::forbidden("写操作必须携带Origin"));
    }
    Ok(())
}

pub fn secure_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn attempt(state: &AppState) -> ApiResult<()> {
    let mut attempts = state
        .0
        .auth_attempts
        .lock()
        .map_err(|_| ApiError::internal())?;
    attempts.retain(|v| v.elapsed() < Duration::from_secs(60));
    if attempts.len() >= 10 {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "登录尝试过于频繁，请稍后再试",
        ));
    }
    attempts.push(Instant::now());
    Ok(())
}

pub async fn auth_status(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let initialized = state.db(|db| {
        Ok(db.query_row("SELECT count(*) FROM admins", [], |r| r.get::<_, i64>(0))? > 0)
    })?;
    Ok(Json(
        json!({"initialized":initialized,"bootstrap_required":!initialized,"version":env!("CARGO_PKG_VERSION")}),
    ))
}

#[derive(Deserialize)]
pub struct Bootstrap {
    username: String,
    password: String,
    bootstrap_token: String,
}
#[derive(Deserialize)]
pub struct Login {
    username: String,
    password: String,
}

fn validate_identity(username: &str, password: &str) -> ApiResult<()> {
    if !(3..=64).contains(&username.len())
        || !username
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.".contains(&c))
    {
        return Err(ApiError::bad(
            "invalid_username",
            "用户名须为3到64位字母、数字、下划线或连字符",
        ));
    }
    if !(12..=256).contains(&password.len()) {
        return Err(ApiError::bad("weak_password", "管理密码须为12到256字节"));
    }
    Ok(())
}

pub async fn bootstrap(
    State(state): State<AppState>,
    Json(body): Json<Bootstrap>,
) -> ApiResult<Response> {
    attempt(&state)?;
    validate_identity(&body.username, &body.password)?;
    let path = state.0.options.data_dir.join("bootstrap-token.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| ApiError::forbidden("请先在SSH终端生成一次性初始化凭证"))?;
    let record: Value = serde_json::from_str(&raw).map_err(|_| ApiError::internal())?;
    if record["expires_at"].as_i64().unwrap_or(0) < now()
        || !secure_eq(
            record["token"].as_str().unwrap_or(""),
            &body.bootstrap_token,
        )
    {
        return Err(ApiError::forbidden("初始化凭证错误或已过期"));
    }
    let password = body.password;
    let hash = tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|h| h.to_string())
            .map_err(|_| ())
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(|_| ApiError::internal())?;
    let id = uuid::Uuid::new_v4().to_string();
    let created = state.db(|db| {
        let tx = db.transaction()?;
        if tx.query_row("SELECT count(*) FROM admins", [], |r| r.get::<_, i64>(0))? > 0 {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO admins(id,username,password_hash,created_at) VALUES(?1,?2,?3,?4)",
            params![id, body.username, hash, now()],
        )?;
        tx.commit()?;
        Ok(true)
    })?;
    if !created {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_initialized",
            "实例已初始化，请登录",
        ));
    }
    let _ = std::fs::remove_file(path);
    state.audit(&id, "admin.bootstrap", &id)?;
    session_response(&state, &id, &body.username)
}

pub async fn login(State(state): State<AppState>, Json(body): Json<Login>) -> ApiResult<Response> {
    attempt(&state)?;
    if body.password.len() > 256 || body.username.len() > 64 {
        return Err(ApiError::unauthorized());
    }
    let user = state.db(|db| {
        Ok(db
            .query_row(
                "SELECT id,password_hash FROM admins WHERE username=?1",
                [&body.username],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?)
    })?;
    let (id, hash) = user.ok_or_else(ApiError::unauthorized)?;
    let valid = tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash)
            .map(|h| {
                Argon2::default()
                    .verify_password(body.password.as_bytes(), &h)
                    .is_ok()
            })
            .unwrap_or(false)
    })
    .await
    .map_err(|_| ApiError::internal())?;
    if !valid {
        return Err(ApiError::unauthorized());
    }
    state.audit(&id, "admin.login", &id)?;
    session_response(&state, &id, &body.username)
}

fn session_response(state: &AppState, id: &str, username: &str) -> ApiResult<Response> {
    let token = random_token();
    let csrf = random_token();
    state.db(|db| {
        db.execute("DELETE FROM sessions WHERE expires_at<=?1", [now()])?;
        db.execute(
            "INSERT INTO sessions(token_hash,user_id,csrf,expires_at) VALUES(?1,?2,?3,?4)",
            params![token_hash(&token), id, csrf, now() + 8 * 3600],
        )?;
        Ok(())
    })?;
    let cookie = format!(
        "dsh_session={token}; HttpOnly; SameSite=Strict; Path=/api/admin/v1; Max-Age=28800{}",
        if state.0.options.cookie_secure {
            "; Secure"
        } else {
            ""
        }
    );
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({"user":{"id":id,"username":username,"role":"admin"},"csrf_token":csrf})),
    )
        .into_response())
}

pub async fn me(admin: Admin) -> Json<Value> {
    Json(
        json!({"user":{"id":admin.id,"username":admin.username,"role":admin.role},"csrf_token":admin.csrf_token}),
    )
}
pub async fn logout(
    State(state): State<AppState>,
    _admin: Admin,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if let Some(token) = headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(';')
        .find_map(|v| v.trim().strip_prefix("dsh_session="))
    {
        state.db(|db| {
            db.execute(
                "DELETE FROM sessions WHERE token_hash=?1",
                [token_hash(token)],
            )?;
            Ok(())
        })?;
    }
    Ok((
        [(
            header::SET_COOKIE,
            "dsh_session=; HttpOnly; SameSite=Strict; Path=/api/admin/v1; Max-Age=0",
        )],
        Json(json!({"ok":true})),
    )
        .into_response())
}
