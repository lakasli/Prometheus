import type { TriggerDefinition } from "./task-triggers";

// This recognizes a review proposal. Only the authenticated server can validate
// dependencies, calendar coverage or authorize persistence and activation.
export function taskTriggerCandidate(
  content: string,
): TriggerDefinition | null {
  if (new TextEncoder().encode(content).length > 65536) return null;
  const code = /```json\s*\n([\s\S]*?)\n```/i.exec(content)?.[1];
  if (!code) return null;
  try {
    const value: unknown = JSON.parse(code);
    const stack: (Set<string> | null)[] = [];
    for (let index = 0; index < code.length; index++) {
      const char = code[index];
      if (char === "{") stack.push(new Set());
      else if (char === "[") stack.push(null);
      else if (char === "}" || char === "]") stack.pop();
      else if (char === '"') {
        const start = index++;
        while (index < code.length) {
          if (code[index] === "\\") index += 2;
          else if (code[index] === '"') break;
          else index++;
        }
        let after = index + 1;
        while (/\s/.test(code[after] || "x")) after++;
        if (code[after] === ":") {
          const key = JSON.parse(code.slice(start, index + 1)) as string;
          const keys = stack.at(-1);
          if (
            !keys ||
            keys.has(key) ||
            ["__proto__", "constructor", "prototype"].includes(key)
          )
            return null;
          keys.add(key);
        }
      }
      if (stack.length > 16) return null;
    }
    let nodes = 0;
    const bounded = (item: unknown, depth: number): boolean => {
      if (++nodes > 2048 || depth > 16) return false;
      return (
        !item ||
        typeof item !== "object" ||
        Object.values(item).every((child) => bounded(child, depth + 1))
      );
    };
    if (
      !bounded(value, 0) ||
      !value ||
      typeof value !== "object" ||
      Array.isArray(value)
    )
      return null;
    const envelope = value as Record<string, unknown>;
    if (
      Object.keys(envelope).length !== 1 ||
      !envelope.task_trigger_draft ||
      typeof envelope.task_trigger_draft !== "object"
    )
      return null;
    const task = envelope.task_trigger_draft as Record<string, unknown>;
    const allowed = [
      "spec",
      "spec_version",
      "title",
      "time_zone",
      "trigger",
      "action",
      "delivery",
      "active_window",
      "policy",
      "budget",
    ];
    if (
      Object.keys(task).length !== allowed.length ||
      Object.keys(task).some((key) => !allowed.includes(key)) ||
      task.spec !== "dsh.task-trigger" ||
      task.spec_version !== "1" ||
      typeof task.title !== "string" ||
      !task.title.trim() ||
      typeof task.time_zone !== "string"
    )
      return null;
    const rule = task.trigger as Record<string, unknown>;
    const action = task.action as Record<string, unknown>;
    const delivery = task.delivery as Record<string, unknown>;
    if (
      !rule ||
      !["once", "interval", "calendar", "event"].includes(String(rule.type)) ||
      !action ||
      !["notify", "assistant_run"].includes(String(action.type)) ||
      delivery?.kind !== "web_inbox" ||
      Object.keys(delivery).length !== 1
    )
      return null;
    const record = (item: unknown): item is Record<string, unknown> =>
      !!item && typeof item === "object" && !Array.isArray(item);
    const only = (item: Record<string, unknown>, keys: string[]) =>
      Object.keys(item).every((key) => keys.includes(key));
    const texts = (item: unknown) =>
      Array.isArray(item) && item.every((value) => typeof value === "string");
    const numbers = (item: unknown) =>
      Array.isArray(item) &&
      item.every(
        (value) => typeof value === "number" && Number.isInteger(value),
      );
    if (
      !record(task.policy) ||
      !record(task.budget) ||
      !record(task.active_window) ||
      !record(action) ||
      !record(rule)
    )
      return null;
    const policyKeys = [
      "misfire",
      "grace_seconds",
      "max_catch_up",
      "overlap",
      "catch_up_window_seconds",
    ];
    const budgetKeys = [
      "daily_occurrences",
      "daily_model_runs",
      "max_output_tokens",
      "run_timeout_seconds",
    ];
    if (
      !only(task.policy, policyKeys) ||
      Object.keys(task.policy).length !== policyKeys.length ||
      !["skip", "coalesce_one", "bounded_catch_up"].includes(
        String(task.policy.misfire),
      ) ||
      !["skip", "coalesce_one"].includes(String(task.policy.overlap)) ||
      ["grace_seconds", "max_catch_up", "catch_up_window_seconds"].some(
        (key) => typeof task.policy![key as never] !== "number",
      )
    )
      return null;
    if (
      !only(task.budget, budgetKeys) ||
      Object.keys(task.budget).length !== budgetKeys.length ||
      budgetKeys.some(
        (key) =>
          typeof (task.budget as Record<string, unknown>)[key] !== "number",
      )
    )
      return null;
    if (
      !only(task.active_window, ["start_at", "end_at"]) ||
      Object.keys(task.active_window).length !== 2 ||
      [task.active_window.start_at, task.active_window.end_at].some(
        (item) => item !== null && typeof item !== "string",
      )
    )
      return null;
    if (action.type === "notify") {
      if (
        !only(action, ["type", "content"]) ||
        typeof action.content !== "string"
      )
        return null;
    } else {
      if (
        !only(action, [
          "type",
          "prompt",
          "skill",
          "persona",
          "output_mode",
          "context_policy",
        ]) ||
        typeof action.prompt !== "string" ||
        action.context_policy !== "isolated" ||
        !["text", "html_drawing", "native_image"].includes(
          String(action.output_mode),
        ) ||
        !record(action.persona)
      )
        return null;
      if (
        action.skill !== null &&
        (!record(action.skill) ||
          !only(action.skill, ["id", "revision"]) ||
          typeof action.skill.id !== "string" ||
          typeof action.skill.revision !== "number")
      )
        return null;
      if (
        !only(action.persona, ["mode", "card_id", "revision"]) ||
        !["instance_snapshot", "character_revision"].includes(
          String(action.persona.mode),
        )
      )
        return null;
    }
    if (rule.type === "once") {
      if (rule.mode === "at") {
        if (!only(rule, ["type", "mode", "at"]) || typeof rule.at !== "string")
          return null;
      } else if (rule.mode === "after") {
        if (
          !only(rule, ["type", "mode", "after_seconds"]) ||
          typeof rule.after_seconds !== "number"
        )
          return null;
      } else return null;
    } else if (rule.type === "interval") {
      if (
        !only(rule, ["type", "every_seconds", "anchor", "first_fire"]) ||
        typeof rule.every_seconds !== "number" ||
        rule.anchor !== "first_enable" ||
        rule.first_fire !== "after_interval"
      )
        return null;
    } else if (rule.type === "calendar") {
      if (
        !only(rule, [
          "type",
          "frequency",
          "at_times",
          "day_class",
          "calendar_id",
          "weekdays",
          "month_days",
          "month_dates",
          "dates",
        ]) ||
        !["daily", "weekly", "monthly", "yearly", "dates"].includes(
          String(rule.frequency),
        ) ||
        !texts(rule.at_times)
      )
        return null;
      if (rule.weekdays !== undefined && !numbers(rule.weekdays)) return null;
      if (
        rule.month_days !== undefined &&
        (!Array.isArray(rule.month_days) ||
          rule.month_days.some(
            (item) => item !== "last" && typeof item !== "number",
          ))
      )
        return null;
      if (
        rule.month_dates !== undefined &&
        (!Array.isArray(rule.month_dates) ||
          rule.month_dates.some(
            (item) =>
              !record(item) ||
              !only(item, ["month", "day"]) ||
              typeof item.month !== "number" ||
              typeof item.day !== "number",
          ))
      )
        return null;
      if (rule.dates !== undefined && !texts(rule.dates)) return null;
    } else if (
      !only(rule, [
        "type",
        "source_id",
        "event_type",
        "filters",
        "cooldown_seconds",
        "day_class",
        "calendar_id",
      ]) ||
      typeof rule.source_id !== "string" ||
      typeof rule.event_type !== "string" ||
      typeof rule.cooldown_seconds !== "number" ||
      !Array.isArray(rule.filters)
    )
      return null;
    return task as unknown as TriggerDefinition;
  } catch {
    return null;
  }
}
