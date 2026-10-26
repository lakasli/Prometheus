//! Explicit memory commands use the durable Run/Inbox/Outbox path without a model.
use crate::{
    error::{ApiError, ApiResult},
    memory::{self, Access, CreateMemory, ListOptions, UpdateMemory, Visibility},
    state::{now, AppState},
    worker::ReplyTarget,
};
use rusqlite::{params, Connection, OptionalExtension};

const HELP: &str = "记忆在同一实例中延续。共享记忆可跨渠道使用；本命令新建的是当前来源账号域的私有记录。\n/memory list [关键词]\n/memory show <ID>\n/memory remember <内容>\n/memory correct <ID>@<版本> <新内容>\n/memory forget <ID>@<版本>\n全库管理、共享发布和JSON导出请使用管理页面的“记忆”。";

pub fn is_command(input: &str) -> bool {
    let input = input.trim();
    input == "/memory"
        || input
            .strip_prefix("/memory")
            .is_some_and(|s| s.starts_with(char::is_whitespace))
}

fn reference(input: &str) -> ApiResult<(&str, i64)> {
    let (id, revision) = input.rsplit_once('@').ok_or_else(|| {
        ApiError::bad(
            "memory_revision_required",
            "请先show查看版本，并使用ID@版本，例如ID@1",
        )
    })?;
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(ApiError::bad("invalid_memory_id", "记忆ID无效"));
    }
    let revision = revision
        .parse::<i64>()
        .ok()
        .filter(|r| *r > 0)
        .ok_or_else(|| ApiError::bad("invalid_memory_revision", "记忆版本须为正整数"))?;
    Ok((id, revision))
}

fn action(
    db: &Connection,
    owner: &str,
    conversation: &str,
    run_id: &str,
    input: &str,
) -> ApiResult<String> {
    let command = input.trim().strip_prefix("/memory").unwrap_or("").trim();
    let (verb, arguments) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    let arguments = arguments.trim();
    if matches!(verb, "remember" | "correct" | "forget") {
        let is_test: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM conversation_character_bindings WHERE conversation_id=?1 AND draft_card_json IS NOT NULL)",
            [conversation], |r|r.get(0),
        ).map_err(|_|ApiError::internal())?;
        if is_test {
            return Err(ApiError::forbidden(
                "角色草稿测试会话不会写入或修改长期记忆，请在正式会话或管理页面操作。",
            ));
        }
        let reply: Option<String> = db
            .query_row("SELECT reply_json FROM runs WHERE id=?1", [run_id], |r| {
                r.get(0)
            })
            .map_err(|_| ApiError::internal())?;
        if let Some(reply) = reply {
            let target: ReplyTarget =
                serde_json::from_str(&reply).map_err(|_| ApiError::internal())?;
            if target.kind != "c2c" {
                return Err(ApiError::forbidden("群聊和频道的记忆写入、纠正与遗忘请由实例管理员管理；私聊可管理自己的来源域记录。"));
            }
        }
    }
    match verb {
        "list" => {
            let page = memory::visible_list(
                db,
                owner,
                &ListOptions {
                    q: (!arguments.is_empty()).then(|| arguments.to_owned()),
                    limit: 8,
                    ..Default::default()
                },
            )?;
            if page.items.is_empty() {
                return Ok("当前可见范围内没有匹配记忆。".into());
            }
            let mut response = format!(
                "记忆：共{}条匹配，显示前{}条。\n",
                page.total,
                page.items.len()
            );
            for item in page.items {
                let excerpt: String = item.content.chars().take(50).collect();
                response.push_str(&format!(
                    "{}@{} [{}] {}\n",
                    item.id,
                    item.revision,
                    if item.visibility == Visibility::Shared {
                        "共享"
                    } else {
                        "私有"
                    },
                    excerpt
                ));
            }
            Ok(response)
        }
        "show" => {
            let item = memory::get_visible(db, owner, arguments)?;
            let content: String = item.content.chars().take(1200).collect();
            let extra = if content.len() < item.content.len() {
                "\n内容较长，完整内容请在管理页面查看。"
            } else {
                ""
            };
            Ok(format!(
                "{}@{} [{}]\n{}{}",
                item.id,
                item.revision,
                if item.visibility == Visibility::Shared {
                    "共享记忆，只能由管理员更改"
                } else {
                    "来源账号域私有"
                },
                content,
                extra
            ))
        }
        "remember" => {
            let source_message_id = db
                .query_row(
                    "SELECT id FROM messages WHERE run_id=?1 AND role='user'",
                    [run_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|_| ApiError::internal())?;
            let item = memory::create_in(
                db,
                Access::Owner(owner),
                &CreateMemory {
                    content: arguments.into(),
                    visibility: Visibility::Private,
                    source_conversation_id: Some(conversation.into()),
                    source_message_id,
                    publish: false,
                },
            )?;
            Ok(format!("已记住：{}@{}。这条记忆保留在当前实例中，对当前来源账号域保密；不会自动向其他用户或群聊公开。", item.id, item.revision))
        }
        "correct" => {
            let (target, content) = arguments.split_once(char::is_whitespace).ok_or_else(|| {
                ApiError::bad(
                    "invalid_memory_command",
                    "用法：/memory correct <ID>@<版本> <新内容>",
                )
            })?;
            let (id, revision) = reference(target)?;
            let item = memory::update_in(
                db,
                Access::Owner(owner),
                id,
                revision,
                &UpdateMemory {
                    content: content.trim().into(),
                    visibility: None,
                    publish: false,
                },
            )?;
            Ok(format!(
                "已纠正：{}@{}。后续对话将使用新版本，受影响的旧上下文已停止使用。",
                item.id, item.revision
            ))
        }
        "forget" => {
            let (id, revision) = reference(arguments)?;
            memory::forget_in(db, Access::Owner(owner), id, revision)?;
            Ok("已从记忆库移除这条记忆及其旧版本，并停止在后续上下文中使用相关旧历史。已发送的聊天消息和离线备份按独立保留策略处理。".into())
        }
        _ => Ok(HELP.into()),
    }
}

#[cfg(test)]
#[path = "memory_command_tests.rs"]
mod integration_tests;

/// Returns true when the Run was handled locally. The mutation, response and
/// outbound delivery record commit together, so restart cannot duplicate effects.
pub fn execute(state: &AppState, run_id: &str) -> ApiResult<bool> {
    let mut db = state.0.db.lock().map_err(|_| ApiError::internal())?;
    let tx = db.transaction().map_err(|_| ApiError::internal())?;
    let (input, owner, conversation, reply, status): (
        String,
        String,
        String,
        Option<String>,
        String,
    ) = tx
        .query_row(
            "SELECT input,owner_id,conversation_id,reply_json,status FROM runs WHERE id=?1",
            [run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(|_| ApiError::internal())?;
    if !is_command(&input) {
        return Ok(false);
    }
    if status != "running" {
        return Ok(true);
    }
    tx.execute(
        "UPDATE runs SET run_kind='memory_command' WHERE id=?1",
        [run_id],
    )
    .map_err(|_| ApiError::internal())?;
    tx.execute_batch("SAVEPOINT memory_command_action")
        .map_err(|_| ApiError::internal())?;
    let response = match action(&tx, &owner, &conversation, run_id, &input) {
        Ok(response) => {
            tx.execute_batch("RELEASE memory_command_action")
                .map_err(|_| ApiError::internal())?;
            response
        }
        Err(error) => {
            tx.execute_batch("ROLLBACK TO memory_command_action; RELEASE memory_command_action")
                .map_err(|_| ApiError::internal())?;
            error.message
        }
    };
    tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'assistant',?4,?5)",
        params![uuid::Uuid::new_v4().to_string(),conversation,run_id,response,now()]).map_err(|_| ApiError::internal())?;
    tx.execute(
        "UPDATE runs SET status='completed',response=?1,error=NULL,updated_at=?2 WHERE id=?3",
        params![response, now(), run_id],
    )
    .map_err(|_| ApiError::internal())?;
    if let Some(reply) = reply {
        let target: ReplyTarget = serde_json::from_str(&reply).map_err(|_| ApiError::internal())?;
        tx.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES(?1,?2,?3,?4,?5,?6,'queued',?7,?8)",
            params![uuid::Uuid::new_v4().to_string(),run_id,target.account_id,target.credential_ref,reply,response,target.deadline,now()]).map_err(|_| ApiError::internal())?;
    }
    crate::weixin_store::queue_reply_in(&tx, run_id, &response)
        .map_err(|_| ApiError::internal())?;
    tx.commit().map_err(|_| ApiError::internal())?;
    state.0.notify.notify_waiters();
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_prefix_and_revision_are_unambiguous() {
        assert!(is_command(" /memory remember something"));
        assert!(!is_command("/memoryside effect"));
        assert!(!is_command("quoted /memory remember something"));
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(reference(&format!("{id}@3")).unwrap(), (id.as_str(), 3));
        assert!(reference(&id).is_err());
        assert!(reference(&format!("{id}@0")).is_err());
    }
}
