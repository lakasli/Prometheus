import type { IncomingMessage } from "node:http";
import type { Admin } from "./auth.ts";
import { bad, fields, integer, missing, revision, string } from "./errors.ts";
import { Channels } from "./channels.ts";
import type { ChannelEndpoints } from "./channels.ts";
import type { Runtime } from "./runtime.ts";
import type { Store } from "./store.ts";
import { Knowledge } from "./knowledge.ts";
import { Automation } from "./automation.ts";
import { Media, Plugins } from "./media.ts";
import { hash } from "./security.ts";
import { ConversationContext } from "./context.ts";
import { Relationships } from "./relationships.ts";
import { Schedule } from "./schedule-work.ts";

export interface FeatureResult {
  data: unknown;
  status?: number;
  raw?: boolean;
  contentType?: string;
  preview?: boolean;
}
export class Features {
  readonly channels: Channels;
  readonly knowledge: Knowledge;
  readonly automation: Automation;
  readonly media: Media;
  readonly plugins: Plugins;
  readonly conversationContext: ConversationContext;
  readonly relationships: Relationships;
  readonly schedule: Schedule;
  constructor(
    readonly store: Store,
    readonly runtime: Runtime,
    allowLoopback = false,
    endpoints?: ChannelEndpoints,
  ) {
    this.channels = new Channels(store, runtime, allowLoopback, endpoints);
    this.knowledge = new Knowledge(store, runtime, allowLoopback);
    this.automation = new Automation(store, runtime, this.knowledge);
    this.media = new Media(store, runtime, this.knowledge, allowLoopback);
    this.plugins = new Plugins(store);
    this.conversationContext = new ConversationContext(
      store,
      runtime,
      this.knowledge,
    );
    this.relationships = new Relationships(store, runtime, this.knowledge);
    this.schedule = new Schedule(
      store,
      runtime,
      this.knowledge,
      allowLoopback,
      () =>
        this.automation.workClockTrusted &&
        this.automation.restStatus().state.phase === "awake",
    );
    this.automation.onScheduleTick = () => this.schedule.tick();
    runtime.records.validateSchedule = (owner, data, id) =>
      this.schedule.checkSimple(owner, data, id);
    runtime.onInvalidation = (owner, change) => {
      this.knowledge.invalidateOwner(owner, change);
      if (change?.action === "forgotten") this.relationships.forget(change.id);
      if (change?.action === "created") this.relationships.created(change.id);
    };
    runtime.getSummary = (conversation) => this.knowledge.summary(conversation);
    runtime.refreshSourceGrants = () => this.knowledge.expireGrants();
    runtime.localCommand = (content, owner, conversation, clientId) =>
      this.localCommand(content, owner, conversation, clientId);
    this.channels.canDeliver = (run) => {
      this.knowledge.expireGrants();
      return (
        this.automation.restStatus().state.phase === "awake" ||
        (!!run &&
          this.store.get("SELECT kind FROM local_runs WHERE run_id=?", run)
            ?.kind === "rest")
      );
    };
    this.knowledge.canRun = (row) => {
      const state = this.automation.restStatus().state;
      return (
        state.phase === "awake" ||
        (state.phase === "consolidating" &&
          String(row.idempotency_key).startsWith(`rest:${state.id}:`))
      );
    };
  }
  start() {
    this.relationships.affinity.start();
    this.media.start();
    this.channels.start();
    this.knowledge.start();
    this.automation.start();
  }
  private localCommand(
    content: string,
    owner: string,
    conversation: string,
    clientId: string,
  ): { response: string; kind: string } | null {
    const conv = this.runtime.ownConversation(conversation, owner),
      scope = String(conv.source_key ?? conv.id),
      group = /^qq:[^:]+:(group|channel):/.test(scope);
    if (/^\/(?:persona|角色)(?:\s|$)/u.test(content))
      return {
        response: `当前姓名：${this.store.personality().personality.name}。每个实例只保留一种性格，请在管理页编辑。`,
        kind: "personality",
      };
    const rest =
      /^\/(sleep|wake)(?:\s+(\d{1,5}))?\s*$/i.exec(content) ??
      (/^(?:快去睡觉啦[！!。]?|去睡觉吧[！!。]?)$/.test(content)
        ? ["", "sleep"]
        : /^(?:起床啦[！!。]?|醒醒[！!。]?)$/.test(content)
          ? ["", "wake"]
          : null);
    if (rest && !group) {
      try {
        const status = this.automation.restStatus(),
          result = this.automation.restControl(
            owner,
            {
              action: rest[1] === "sleep" ? "sleep" : "wake",
              ...(rest[2] ? { duration_seconds: Number(rest[2]) } : {}),
              source_key: scope,
              request_id: `message:${hash(scope + clientId)}`,
            },
            Number(status.state.revision),
          );
        return {
          response:
            rest[1] === "sleep"
              ? `我先休息一会儿，已安排在上海时间${new Date(Number(result.state.wake_at) * 1000).toLocaleString("zh-CN", { timeZone: "Asia/Shanghai" })}醒来。`
              : "我醒啦。",
          kind: "rest",
        };
      } catch (error) {
        return {
          response:
            error instanceof Error ? error.message : "这次作息请求没有完成。",
          kind: "rest",
        };
      }
    }
    if (!/^\/memory(?:\s|$)/.test(content)) return null;
    const command = content.slice("/memory".length).trim(),
      operation = command.split(/\s/, 1)[0];
    try {
      if (operation === "list") {
        const page = this.runtime.records.list(
          "memory",
          owner,
          new URLSearchParams({ limit: "20" }),
        );
        return {
          response: page.items.length
            ? page.items
                .map(
                  (item) =>
                    `${item.id}@${item.revision} ${String(item.data.content)}`,
                )
                .join("\n")
            : "这里还没有明确保存的私有记忆。",
          kind: "memory",
        };
      }
      if (operation === "show") {
        const item = this.runtime.records.get(
          "memory",
          command.slice(4).trim(),
          owner,
        );
        return {
          response: `${item.id}@${item.revision} ${item.data.content}`,
          kind: "memory",
        };
      }
      if (group)
        return {
          response:
            "群聊与频道不接受记忆写入或遗忘命令，请在私聊或管理页处理。",
          kind: "memory",
        };
      if (operation === "remember") {
        const item = this.runtime.records.save("memory", owner, {
          data: { content: command.slice(8).trim() },
        });
        return {
          response: `已私有保存：${item.id}@${item.revision}`,
          kind: "memory",
        };
      }
      const match =
        /^(correct|forget)\s+([a-f0-9-]{36})@(\d+)(?:\s+([\s\S]+))?$/.exec(
          command,
        );
      if (match) {
        const rev = integer(
          Number(match[3]),
          "记忆版本",
          1,
          Number.MAX_SAFE_INTEGER,
        );
        if (match[1] === "forget") {
          this.runtime.records.forget("memory", match[2]!, owner, rev);
          return {
            response:
              "这条记忆与依赖它的派生资料已遗忘；原始聊天与离线备份保留。",
            kind: "memory",
          };
        }
        const item = this.runtime.records.save(
          "memory",
          owner,
          { data: { content: match[4] ?? "" } },
          match[2],
          rev,
        );
        return {
          response: `已更正：${item.id}@${item.revision}`,
          kind: "memory",
        };
      }
      return {
        response:
          "可用命令：/memory remember 内容、list、show ID、correct ID@版本 新内容、forget ID@版本。",
        kind: "memory",
      };
    } catch (error) {
      return {
        response: error instanceof Error ? error.message : "记忆操作没有完成。",
        kind: "memory",
      };
    }
  }
  async stop() {
    await this.schedule.stop();
    this.relationships.affinity.stop();
    this.media.stop();
    this.automation.stop();
    await this.channels.stop();
    await this.knowledge.stop();
  }
  matches(path: string) {
    return /^\/(schedule|channels|knowledge|automation|context|rest|media|plugins|relationships|qq\/api)(?:\/|$)/.test(
      path,
    );
  }
  async route(
    path: string,
    method: string,
    admin: Admin,
    request: IncomingMessage,
    url: URL,
    readBody: () => Promise<unknown>,
  ): Promise<FeatureResult> {
    if (path === "/schedule/calendar" && method === "GET")
      return { data: this.schedule.calendar(admin.id, url.searchParams) };
    if (path === "/schedule/activities" && method === "GET")
      return { data: this.schedule.list(admin.id, url.searchParams) };
    if (path === "/schedule/activities" && method === "POST")
      return {
        status: 201,
        data: this.schedule.create(
          admin.id,
          await readBody(),
          request.headers["idempotency-key"],
        ),
      };
    if (path === "/schedule/proposals" && method === "POST")
      return {
        status: 201,
        data: this.schedule.create(
          admin.id,
          await readBody(),
          request.headers["idempotency-key"],
          true,
        ),
      };
    if (path === "/schedule/activities/preview" && method === "POST")
      return { data: this.schedule.preview(admin.id, await readBody()) };
    const scheduled =
      /^\/schedule\/activities\/([a-f0-9-]{36})(?:\/(history|preview|move-preview|approve|pause|resume|cancel|attempts))?$/.exec(
        path,
      );
    if (scheduled) {
      const id = scheduled[1]!;
      if (method === "GET") {
        if (!scheduled[2]) return { data: this.schedule.detail(admin.id, id) };
        if (scheduled[2] === "history")
          return { data: this.schedule.history(admin.id, id) };
        if (scheduled[2] === "attempts") {
          this.schedule.detail(admin.id, id);
          return {
            data: { items: this.schedule.attempts(admin.id, id, "activity") },
          };
        }
      }
      if (method === "PATCH" && !scheduled[2])
        return {
          data: this.schedule.change(
            admin.id,
            id,
            revision(request.headers["if-match"]),
            await readBody(),
          ),
        };
      if (method === "POST") {
        const expected = revision(request.headers["if-match"]),
          data = await readBody();
        if (scheduled[2] === "preview")
          return { data: this.schedule.preview(admin.id, data, id, expected) };
        if (scheduled[2] === "move-preview")
          return {
            data: this.schedule.movePreview(admin.id, id, expected, data),
          };
        if (scheduled[2] === "approve")
          return { data: this.schedule.approve(admin.id, id, expected, data) };
        const state = {
          pause: "paused",
          resume: "enabled",
          cancel: "cancelled",
        }[scheduled[2] ?? ""];
        if (state) {
          const input = fields(data, ["scope", "nominal_at"]);
          return {
            data: this.schedule.change(admin.id, id, expected, {
              ...input,
              state,
            }),
          };
        }
      }
    }
    if (path === "/schedule/resources" && method === "GET")
      return { data: this.schedule.resources(admin.id) };
    if (path === "/schedule/resources" && method === "POST")
      return {
        status: 201,
        data: this.schedule.saveResource(admin.id, await readBody()),
      };
    const resource =
      /^\/schedule\/resources\/([a-f0-9-]{36})(?:\/(revoke))?$/.exec(path);
    if (resource) {
      if (method === "GET" && !resource[2])
        return { data: this.schedule.resourceDetail(admin.id, resource[1]!) };
      if (method === "POST" && resource[2]) {
        fields(await readBody(), []);
        return {
          data: this.schedule.revokeResource(
            admin.id,
            resource[1]!,
            revision(request.headers["if-match"]),
          ),
        };
      }
    }
    if (path === "/schedule/autonomy-grants" && method === "GET")
      return { data: this.schedule.grants(admin.id) };
    if (path === "/schedule/autonomy-grants" && method === "POST")
      return {
        status: 201,
        data: this.schedule.saveGrant(admin.id, await readBody()),
      };
    const grant =
      /^\/schedule\/autonomy-grants\/([a-f0-9-]{36})(?:\/(revoke))?$/.exec(
        path,
      );
    if (grant) {
      if (method === "GET" && !grant[2])
        return { data: this.schedule.grantDetail(admin.id, grant[1]!) };
      if (method === "PATCH" && !grant[2])
        return {
          data: this.schedule.saveGrant(
            admin.id,
            await readBody(),
            grant[1]!,
            revision(request.headers["if-match"]),
          ),
        };
      if (method === "POST" && grant[2]) {
        fields(await readBody(), []);
        return {
          data: this.schedule.revokeGrant(
            admin.id,
            grant[1]!,
            revision(request.headers["if-match"]),
          ),
        };
      }
    }
    if (path === "/schedule/plans" && method === "GET")
      return { data: this.schedule.plans(admin.id) };
    if (path === "/schedule/plans" && method === "POST")
      return {
        status: 201,
        data: this.schedule.savePlan(admin.id, await readBody()),
      };
    const plan = /^\/schedule\/plans\/([a-f0-9-]{36})(?:\/(preview))?$/.exec(
      path,
    );
    if (plan) {
      if (method === "GET" && !plan[2])
        return { data: this.schedule.planDetail(admin.id, plan[1]!) };
      if (method === "PATCH" && !plan[2])
        return {
          data: this.schedule.savePlan(
            admin.id,
            await readBody(),
            plan[1]!,
            revision(request.headers["if-match"]),
          ),
        };
      if (method === "GET" && plan[2])
        return { data: this.schedule.previewPlan(admin.id, plan[1]!) };
    }
    if (path === "/relationships/people" && method === "GET")
      return {
        data: {
          items: this.relationships.people(admin.id),
          legacy_mappings: this.relationships.legacyMappings(admin.id),
        },
      };
    if (path === "/relationships/policy" && method === "GET")
      return { data: this.relationships.policy(admin.id) };
    if (path === "/relationships/policy" && method === "PUT")
      return {
        data: this.relationships.savePolicy(
          admin.id,
          integer(
            Number(request.headers["if-match"]?.replaceAll('"', "") ?? -1),
            "版本",
            0,
            Number.MAX_SAFE_INTEGER,
          ),
          await readBody(),
        ),
      };
    const person =
      /^\/relationships\/people\/([a-f0-9-]{36})\/(event|merge|split)$/.exec(
        path,
      );
    if (person && method === "POST")
      return {
        data:
          person[2] === "event"
            ? this.relationships.add(
                admin.id,
                person[1]!,
                revision(request.headers["if-match"]),
                await readBody(),
              )
            : person[2] === "merge"
              ? this.relationships.merge(admin.id, person[1]!, await readBody())
              : this.relationships.split(
                  admin.id,
                  person[1]!,
                  await readBody(),
                ),
      };
    if (path === "/relationships/policy/history" && method === "GET")
      return { data: this.relationships.affinity.history(admin.id) };
    if (path === "/relationships/policy/preview" && method === "POST")
      return {
        data: this.relationships.affinity.preview(admin.id, await readBody()),
      };
    if (path === "/context/policy" && method === "GET")
      return { data: this.conversationContext.policy() };
    if (path === "/context/policy" && method === "PUT")
      return {
        data: this.conversationContext.save(
          integer(
            Number(request.headers["if-match"]?.replaceAll('"', "") ?? -1),
            "版本",
            0,
            Number.MAX_SAFE_INTEGER,
          ),
          await readBody(),
        ),
      };
    const annotations = /^\/context\/conversations\/([a-f0-9-]{36})$/.exec(
      path,
    );
    if (annotations) {
      if (method === "GET")
        return {
          data: {
            items: this.conversationContext.list(admin.id, annotations[1]!),
          },
        };
      if (method === "POST")
        return {
          status: 201,
          data: this.conversationContext.create(
            admin.id,
            annotations[1]!,
            await readBody(),
          ),
        };
    }
    const annotation = /^\/context\/annotations\/([a-f0-9-]{36})$/.exec(path);
    if (annotation && method === "DELETE")
      return {
        data: this.conversationContext.remove(
          admin.id,
          annotation[1]!,
          revision(request.headers["if-match"]),
        ),
      };
    const selection = /^\/context\/runs\/([a-f0-9-]{36})$/.exec(path);
    if (selection && method === "GET")
      return {
        data: this.conversationContext.selection(admin.id, selection[1]!),
      };
    const qqOperation = /^\/qq\/api\/operations\/([a-z0-9_]+)$/.exec(path);
    if (qqOperation && method === "POST")
      return {
        data: await this.channels.operate(
          admin.id,
          qqOperation[1]!,
          await readBody(),
        ),
      };
    if (path === "/media/policy" && method === "GET")
      return { data: this.media.policy() };
    if (path === "/media/policy" && method === "PUT")
      return {
        data: this.media.savePolicy(
          integer(
            Number(request.headers["if-match"]?.replaceAll('"', "") ?? -1),
            "版本",
            0,
            Number.MAX_SAFE_INTEGER,
          ),
          await readBody(),
        ),
      };
    if (path === "/media/assets" && method === "POST")
      return {
        status: 201,
        data: this.media.upload(admin.id, await readBody()),
      };
    if (path === "/media/generate" && method === "POST")
      return {
        status: 201,
        data: await this.media.generate(admin.id, await readBody()),
      };
    const asset = /^\/media\/assets\/([a-f0-9-]{36})(?:\/(chat))?$/.exec(path);
    if (asset) {
      if (method === "GET") {
        const value = this.media.asset(admin.id, asset[1]!);
        return {
          data: Buffer.from(value.bytes),
          raw: true,
          contentType: value.mime,
        };
      }
      if (method === "POST" && asset[2])
        return {
          status: 202,
          data: this.media.submit(admin.id, asset[1]!, await readBody()),
        };
    }
    if (path === "/plugins" && method === "GET")
      return {
        data: {
          items: this.plugins.list(admin.id),
          execution_available: false,
        },
      };
    if (path === "/plugins" && method === "POST")
      return {
        status: 201,
        data: this.plugins.create(admin.id, await readBody()),
      };
    const plugin =
      /^\/plugins\/([a-f0-9-]{36})\/(test|approve|activate|execute)$/.exec(
        path,
      );
    if (plugin && method === "POST") {
      if (plugin[2] === "test")
        return {
          data: this.plugins.test(
            admin.id,
            plugin[1]!,
            revision(request.headers["if-match"]),
          ),
        };
      this.plugins.execute();
    }
    if (path === "/plugins/execute") this.plugins.execute();
    const preview = /^\/knowledge\/jobs\/([a-f0-9-]{36})\/preview$/.exec(path);
    if (preview && method === "GET") {
      const job = this.knowledge.job(admin.id, preview[1]!);
      if (
        job.kind !== "drawing_html" ||
        !job.output ||
        !["review_pending", "approved"].includes(String(job.status))
      )
        throw missing();
      return {
        data: String(job.output.html),
        raw: true,
        contentType: "text/html; charset=utf-8",
        preview: true,
      };
    }
    if (path === "/automation/triggers" && method === "GET")
      return { data: { items: this.automation.list(admin.id) } };
    if (path === "/automation/triggers" && method === "POST")
      return {
        status: 201,
        data: this.automation.save(admin.id, await readBody()),
      };
    const trigger =
      /^\/automation\/triggers\/([a-f0-9-]{36})(?:\/(preview|enable))?$/.exec(
        path,
      );
    if (trigger) {
      const id = trigger[1]!;
      if (trigger[2] === "preview" && method === "GET")
        return { data: this.automation.preview(admin.id, id) };
      if (trigger[2] === "enable" && method === "PUT")
        return {
          data: this.automation.enable(
            admin.id,
            id,
            revision(request.headers["if-match"]),
            await readBody(),
          ),
        };
      if (method === "PUT")
        return {
          data: this.automation.save(
            admin.id,
            await readBody(),
            id,
            revision(request.headers["if-match"]),
          ),
        };
      if (method === "DELETE")
        return {
          data: this.automation.remove(
            admin.id,
            id,
            revision(request.headers["if-match"]),
          ),
        };
    }
    if (path === "/automation/results" && method === "GET")
      return { data: { items: this.automation.results(admin.id) } };
    if (path === "/automation/calendar" && method === "GET")
      return {
        data: this.schedule.calendar(admin.id, url.searchParams),
      };
    if (path === "/rest/status" && method === "GET")
      return { data: this.automation.restStatus() };
    if (path === "/rest/policy" && method === "GET")
      return { data: this.automation.restPolicy() };
    if (path === "/rest/policy" && method === "PUT")
      return {
        data: this.automation.saveRestPolicy(
          integer(
            Number(request.headers["if-match"]?.replaceAll('"', "") ?? -1),
            "版本",
            0,
            Number.MAX_SAFE_INTEGER,
          ),
          await readBody(),
        ),
      };
    if (path === "/rest/control" && method === "POST")
      return {
        data: this.automation.restControl(
          admin.id,
          await readBody(),
          integer(
            Number(request.headers["if-match"]?.replaceAll('"', "") ?? -1),
            "状态版本",
            0,
            Number.MAX_SAFE_INTEGER,
          ),
        ),
      };
    if (path === "/rest/history" && method === "GET")
      return {
        data: {
          items: this.store.all(
            "SELECT * FROM rest_cycles ORDER BY started_at DESC LIMIT 50",
          ),
        },
      };
    if (path === "/knowledge/sources" && method === "GET")
      return { data: { items: this.knowledge.sources(admin.id) } };
    if (path === "/knowledge/source/messages" && method === "GET")
      return {
        data: {
          items: this.knowledge.messages(
            admin.id,
            url.searchParams.get("source_key") ?? "",
          ),
        },
      };
    if (path === "/knowledge/source/grant" && method === "PUT") {
      const body = fields(await readBody(), ["source_key", "grant"]);
      return {
        data: this.knowledge.setGrant(
          admin.id,
          string(body.source_key, "来源", 512, 1),
          integer(
            Number(request.headers["if-match"]?.replaceAll('"', "") ?? -1),
            "版本",
            0,
            Number.MAX_SAFE_INTEGER,
          ),
          body.grant,
        ),
      };
    }
    if (path === "/knowledge/jobs" && method === "GET")
      return { data: { items: this.knowledge.jobs(admin.id) } };
    if (path === "/knowledge/jobs" && method === "POST")
      return {
        status: 202,
        data: this.knowledge.enqueue(admin.id, await readBody()),
      };
    const job =
      /^\/knowledge\/jobs\/([a-f0-9-]{36})(?:\/(review|retry))?$/.exec(path);
    if (job) {
      const id = job[1]!;
      if (method === "GET") return { data: this.knowledge.job(admin.id, id) };
      const expected = revision(request.headers["if-match"]);
      if (method === "DELETE")
        return { data: this.knowledge.cancel(admin.id, id, expected) };
      if (method === "POST" && job[2] === "retry")
        return { data: this.knowledge.retry(admin.id, id, expected) };
      if (method === "POST" && job[2] === "review") {
        const body = fields(await readBody(), ["approve"]);
        if (typeof body.approve !== "boolean") throw bad("审核选择无效");
        return {
          data: this.knowledge.review(admin.id, id, expected, body.approve),
        };
      }
    }
    if (path === "/knowledge/policy" && method === "GET")
      return {
        data: this.knowledge.policy("knowledge", {
          automatic_diary: false,
          automatic_summary: false,
          max_daily_calls: 32,
        }),
      };
    if (path === "/knowledge/policy" && method === "PUT") {
      const body = fields(await readBody(), [
        "automatic_diary",
        "automatic_summary",
        "max_daily_calls",
      ]);
      if (
        typeof body.automatic_diary !== "boolean" ||
        typeof body.automatic_summary !== "boolean"
      )
        throw bad("自动整理开关无效");
      return {
        data: this.knowledge.setPolicy(
          "knowledge",
          integer(
            Number(request.headers["if-match"]?.replaceAll('"', "") ?? -1),
            "版本",
            0,
            Number.MAX_SAFE_INTEGER,
          ),
          {
            ...body,
            max_daily_calls: integer(body.max_daily_calls, "每日预算", 1, 128),
          },
        ),
      };
    }
    if (path === "/channels/status" && method === "GET")
      return { data: this.channels.status() };
    if (path === "/channels/deliveries" && method === "GET")
      return { data: { items: this.channels.deliveries(admin.id) } };
    if (path === "/channels/weixin/login" && method === "POST") {
      const body = fields(await readBody(), ["expected_revision"]);
      return {
        status: 202,
        data: await this.channels.createWeixinLogin(
          admin.id,
          integer(
            body.expected_revision,
            "账户版本",
            0,
            Number.MAX_SAFE_INTEGER,
          ),
        ),
      };
    }
    const login =
      /^\/channels\/weixin\/login\/([a-f0-9-]{36})(?:\/(verify))?$/.exec(path);
    if (login) {
      if (method === "GET")
        return { data: this.channels.loginState(login[1]!, admin.id) };
      if (method === "DELETE")
        return {
          data: this.channels.cancelLogin(
            login[1]!,
            admin.id,
            revision(request.headers["if-match"]),
          ),
        };
      if (method === "POST" && login[2]) {
        const body = fields(await readBody(), ["code"]);
        return {
          data: this.channels.verifyLogin(
            login[1]!,
            admin.id,
            revision(request.headers["if-match"]),
            string(body.code, "验证码", 16, 1),
          ),
        };
      }
    }
    if (path === "/qq/api/catalog" && method === "GET")
      return { data: this.channels.catalog() };
    void url;
    throw missing();
  }
  async webhook(request: IncomingMessage): Promise<unknown> {
    const chunks: Buffer[] = [];
    let size = 0;
    for await (const chunk of request) {
      size += Buffer.byteLength(chunk);
      if (size > 128 * 1024) throw bad("QQ 事件过大");
      chunks.push(Buffer.from(chunk));
    }
    return this.channels.webhook(request, Buffer.concat(chunks));
  }
}
