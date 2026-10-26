//! Explicit cross-account identity equivalence. Membership grants no source access.
//! Original people, field ACLs, relationship endpoints and affinity ledgers never move.
use super::{
    administrative, audit, bump, check_revision, invalidate_person_context, person, Access,
    DbResult, Person,
};
use crate::{
    error::{ApiError, ApiResult},
    state::now,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkInput {
    pub other_person_id: String,
    pub other_revision: i64,
    pub confirmed: bool,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct IdentityGroup {
    pub id: String,
    pub revision: i64,
    pub members: Vec<Person>,
    pub mapping_scope: &'static str,
}
pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS relationship_identity_groups(id TEXT PRIMARY KEY,revision INTEGER NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS relationship_identity_group_members(person_id TEXT PRIMARY KEY REFERENCES relationship_people(id),group_id TEXT NOT NULL REFERENCES relationship_identity_groups(id),linked_at INTEGER NOT NULL);
 CREATE INDEX IF NOT EXISTS relationship_group_members ON relationship_identity_group_members(group_id,person_id);
 CREATE TABLE IF NOT EXISTS relationship_identity_group_history(id INTEGER PRIMARY KEY,group_id TEXT NOT NULL,person_id TEXT NOT NULL,other_person_id TEXT,action TEXT NOT NULL,actor TEXT NOT NULL,reason TEXT,created_at INTEGER NOT NULL);")?;
    Ok(())
}
fn group_id(db: &Connection, id: &str) -> ApiResult<Option<String>> {
    db.query_row(
        "SELECT group_id FROM relationship_identity_group_members WHERE person_id=?1",
        [id],
        |r| r.get(0),
    )
    .optional()
    .api()
}
pub fn get(
    db: &Connection,
    access: Access<'_>,
    person_id: &str,
) -> ApiResult<Option<IdentityGroup>> {
    person(db, access, person_id)?;
    let Some(id) = group_id(db, person_id)? else {
        return Ok(None);
    };
    let revision = db
        .query_row(
            "SELECT revision FROM relationship_identity_groups WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .api()?;
    let mut st=db.prepare("SELECT p.id FROM relationship_identity_group_members m JOIN relationship_people p ON p.id=m.person_id WHERE m.group_id=?1 AND p.deleted_at IS NULL AND (?2 IS NULL OR p.visibility='shared' OR p.source_owner_id=?2) ORDER BY m.linked_at,p.id").api()?;
    let ids = st
        .query_map(params![id, access.owner()], |r| r.get::<_, String>(0))
        .api()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .api()?;
    let members = ids
        .into_iter()
        .map(|id| person(db, access, &id))
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Some(IdentityGroup {
        id,
        revision,
        members,
        mapping_scope: "identity_only_no_acl_grant",
    }))
}
/// Only visible IDs are returned; a linked private identity in another domain stays absent.
pub fn visible_roots(
    db: &Connection,
    access: Access<'_>,
    person_id: &str,
) -> ApiResult<Vec<String>> {
    let p = person(db, access, person_id)?;
    let mut ids = vec![p.id.clone()];
    if let Some(group) = get(db, access, &p.id)? {
        ids.extend(group.members.into_iter().map(|p| p.id));
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}
pub fn link(
    db: &mut Connection,
    access: Access<'_>,
    id: &str,
    expected: i64,
    b: &LinkInput,
) -> ApiResult<IdentityGroup> {
    administrative(access)?;
    super::text(&b.reason, 2000, "跨账号身份核验依据")?;
    if !b.confirmed || id == b.other_person_id {
        return Err(ApiError::bad(
            "identity_group_confirmation",
            "须核验两个不同人物档案并明确确认同一真实人",
        ));
    }
    let tx = db.transaction().api()?;
    let p = person(&tx, access, id)?;
    let other = person(&tx, access, &b.other_person_id)?;
    check_revision(&p, expected)?;
    check_revision(&other, b.other_revision)?;
    if p.merged_into.is_some() || other.merged_into.is_some() {
        return Err(ApiError::bad(
            "identity_group_merged_person",
            "请使用各分区未合并的主档案建立跨账号身份组",
        ));
    }
    let a = group_id(&tx, id)?;
    let c = group_id(&tx, &other.id)?;
    if a.is_some() && c.is_some() && a != c {
        return Err(ApiError::bad(
            "identity_groups_already_separate",
            "两个档案已有不同身份组，请核对后先解除旧映射，不能自动合并整组",
        ));
    }
    if a.is_some() && a == c {
        return get(&tx, access, id)?.ok_or_else(ApiError::internal);
    }
    let group = a.or(c).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let t = now();
    tx.execute("INSERT OR IGNORE INTO relationship_identity_groups(id,revision,created_at,updated_at) VALUES(?1,0,?2,?2)",params![group,t]).api()?;
    let size: i64 = tx
        .query_row(
            "SELECT count(*) FROM relationship_identity_group_members WHERE group_id=?1",
            [&group],
            |r| r.get(0),
        )
        .api()?;
    if size >= 10 {
        return Err(ApiError::bad(
            "identity_group_limit",
            "同一人物身份组最多10个账号档案",
        ));
    }
    tx.execute("INSERT OR IGNORE INTO relationship_identity_group_members(person_id,group_id,linked_at) VALUES(?1,?2,?3)",params![id,group,t]).api()?;
    tx.execute("INSERT OR IGNORE INTO relationship_identity_group_members(person_id,group_id,linked_at) VALUES(?1,?2,?3)",params![other.id,group,t]).api()?;
    tx.execute(
        "UPDATE relationship_identity_groups SET revision=revision+1,updated_at=?1 WHERE id=?2",
        params![t, group],
    )
    .api()?;
    let members = {
        let mut st = tx
            .prepare("SELECT person_id FROM relationship_identity_group_members WHERE group_id=?1")
            .api()?;
        let ids = st
            .query_map([&group], |r| r.get::<_, String>(0))
            .api()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .api()?;
        ids
    };
    for member in members {
        invalidate_person_context(&tx, &member).map_err(|_| ApiError::internal())?;
        bump(&tx, &member)?;
    }
    tx.execute("INSERT INTO relationship_identity_group_history(group_id,person_id,other_person_id,action,actor,reason,created_at) VALUES(?1,?2,?3,'link',?4,?5,?6)",params![group,id,other.id,access.actor(),b.reason,t]).api()?;
    audit(&tx, access.actor(), "relationships.identity_group.link", id)?;
    let result = get(&tx, access, id)?.ok_or_else(ApiError::internal)?;
    tx.commit().api()?;
    Ok(result)
}
pub fn unlink(
    db: &mut Connection,
    access: Access<'_>,
    id: &str,
    expected: i64,
    b: &super::ConfirmChange,
) -> ApiResult<serde_json::Value> {
    administrative(access)?;
    super::text(&b.reason, 2000, "身份组拆分依据")?;
    if !b.confirmed {
        return Err(ApiError::bad(
            "identity_group_confirmation",
            "身份拆分须明确确认",
        ));
    }
    let tx = db.transaction().api()?;
    let p = person(&tx, access, id)?;
    check_revision(&p, expected)?;
    let group = group_id(&tx, id)?
        .ok_or_else(|| ApiError::bad("identity_group_missing", "该档案没有跨账号身份映射"))?;
    detach_in(&tx, id).map_err(|_| ApiError::internal())?;
    tx.execute("INSERT INTO relationship_identity_group_history(group_id,person_id,action,actor,reason,created_at) VALUES(?1,?2,'unlink',?3,?4,?5)",params![group,id,access.actor(),b.reason,now()]).api()?;
    audit(
        &tx,
        access.actor(),
        "relationships.identity_group.unlink",
        id,
    )?;
    tx.commit().api()?;
    Ok(
        serde_json::json!({"id":id,"revision":expected+1,"identity_group":null,"sources_moved":false}),
    )
}
pub fn detach_in(db: &Connection, id: &str) -> anyhow::Result<()> {
    let group: Option<String> = db
        .query_row(
            "SELECT group_id FROM relationship_identity_group_members WHERE person_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(group) = group else { return Ok(()) };
    let members = {
        let mut st = db.prepare(
            "SELECT person_id FROM relationship_identity_group_members WHERE group_id=?1",
        )?;
        let ids = st
            .query_map([&group], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    for member in members {
        invalidate_person_context(db, &member)?;
        db.execute(
            "UPDATE relationship_people SET revision=revision+1,updated_at=?1 WHERE id=?2",
            params![now(), member],
        )?;
    }
    db.execute(
        "DELETE FROM relationship_identity_group_members WHERE person_id=?1",
        [id],
    )?;
    let count: i64 = db.query_row(
        "SELECT count(*) FROM relationship_identity_group_members WHERE group_id=?1",
        [&group],
        |r| r.get(0),
    )?;
    if count < 2 {
        db.execute(
            "DELETE FROM relationship_identity_group_members WHERE group_id=?1",
            [&group],
        )?;
    }
    db.execute(
        "UPDATE relationship_identity_groups SET revision=revision+1,updated_at=?1 WHERE id=?2",
        params![now(), group],
    )?;
    Ok(())
}
