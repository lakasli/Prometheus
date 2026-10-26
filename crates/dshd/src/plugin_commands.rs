//! Explicit chat invocation of an owner's previously approved active tools.
//! This command never installs, approves, activates, or grants channel identity.
use crate::{plugin_factory, state::AppState};
use rusqlite::OptionalExtension;
use tokio_util::sync::CancellationToken;

pub fn is_command(input: &str) -> bool {
    input.trim_start().split_whitespace().next() == Some("/plugin")
}
pub async fn execute(
    state: &AppState,
    run_id: &str,
    cancel: &CancellationToken,
) -> Result<Option<String>, String> {
    let value = state
        .db(|db| {
            Ok(db
                .query_row(
                    "SELECT owner_id,input FROM runs WHERE id=?1",
                    [run_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?)
        })
        .map_err(|_| "插件调用上下文不可用".to_owned())?;
    let Some((owner, input)) = value else {
        return Err("插件调用上下文不存在".into());
    };
    if !is_command(&input) {
        return Ok(None);
    }
    let rest = input
        .trim_start()
        .strip_prefix("/plugin")
        .unwrap_or("")
        .trim();
    if rest.is_empty() || rest == "list" {
        let tools = plugin_factory::active_tools(state, &owner)
            .map_err(|_| "无法读取当前身份的已启用插件".to_owned())?;
        if tools.is_empty() {
            return Ok(Some(
                "当前聊天身份尚未启用插件；管理账户的私有插件不会自动授权给QQ或其他聊天身份。"
                    .into(),
            ));
        }
        let list = tools
            .iter()
            .filter_map(|tool| {
                Some(format!(
                    "{}（{}）",
                    tool["id"].as_str()?,
                    tool["version"].as_str()?
                ))
            })
            .collect::<Vec<_>>()
            .join("、");
        return Ok(Some(format!(
            "当前可用插件：{list}。使用 /plugin 插件ID JSON输入 调用。"
        )));
    }
    let Some((id, args)) = rest.split_once(char::is_whitespace) else {
        return Ok(Some(
            "使用 /plugin 查看插件，或使用 /plugin 插件ID JSON输入 调用。".into(),
        ));
    };
    if args.len() > crate::plugin_component_worker::MAX_INPUT {
        return Err("插件参数超过16KiB限制".into());
    }
    let value =
        serde_json::from_str(args.trim()).map_err(|_| "插件参数必须是有效JSON".to_owned())?;
    let result =
        plugin_factory::invoke_active_with_run(state, &owner, id, value, cancel, Some(run_id))
            .await
            .map_err(|error| {
                match error.code {
                    "plugin_revision_conflict" => "插件版本已变化，请重新调用",
                    "plugin_input_contract_invalid" => "插件输入不符合已启用版本的要求",
                    "plugin_worker_busy" => "插件工作器繁忙，请稍后重试",
                    "plugin_call_failed" | "plugin_output_contract_invalid" => {
                        "插件执行失败，此版本已隔离"
                    }
                    _ => "当前聊天身份无此可用插件，或插件工作器不可用",
                }
                .to_owned()
            })?;
    Ok(Some(format!("插件 {id} 的不可信数据输出：{}", result)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_requires_exact_prefix() {
        assert!(is_command("/plugin"));
        assert!(is_command(" /plugin user.sum [1,2]"));
        assert!(!is_command("/plugins"));
        assert!(!is_command("引用 /plugin user.sum [1,2]"));
    }
}
