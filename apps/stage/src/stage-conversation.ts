import { computed, onBeforeUnmount, onMounted, ref, shallowRef } from "vue";

interface Conversation {
  id: string;
  title: string;
}

interface Session {
  user: { id: string };
  csrf_token: string;
}

interface FeedItem {
  id: string;
  role: "user" | "assistant";
  text: string;
  createdAt: number;
  expiresAt: number;
  expression?: { id: string; expiresAt: number };
}

interface Feed {
  now: number;
  items: FeedItem[];
}

interface TimedExpression {
  id: string;
  createdAt: number;
  messageId: string;
  deadline: number;
}

class StageRequestError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

async function request<T>(
  path: string,
  signal: AbortSignal,
  mutation?: { csrf: string; body: object },
): Promise<T> {
  const response = await fetch(`/api/admin/v1${path}`, {
    method: mutation ? "POST" : "GET",
    headers: mutation
      ? {
          accept: "application/json",
          "content-type": "application/json",
          "x-csrf-token": mutation.csrf,
        }
      : { accept: "application/json" },
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    signal: AbortSignal.any([signal, AbortSignal.timeout(10_000)]),
    body: mutation && JSON.stringify(mutation.body),
  });
  const data = await response.json().catch(() => null);
  if (!response.ok)
    throw new StageRequestError(
      response.status,
      data?.error?.message ?? "无法连接对话服务。",
    );
  if (!data) throw new Error("对话服务返回了无效响应。");
  return data as T;
}

function errorText(error: unknown): string {
  return error instanceof StageRequestError
    ? error.message
    : "对话连接暂时不可用。";
}

export function useStageConversation(allowedExpressions: ReadonlySet<string>) {
  const status = ref<"connecting" | "signed-in" | "signed-out" | "unavailable">(
    "connecting",
  );
  const conversations = shallowRef<Conversation[]>([]);
  const selected = ref("");
  const draft = ref("");
  const busy = ref(false);
  const error = ref("");
  const feedError = ref("");
  const bubbles = shallowRef<readonly FeedItem[]>([]);
  const expression = ref<string>();
  const canSend = computed(() => !!draft.value.trim() && !busy.value);

  let session: Session | undefined;
  let requests = new AbortController();
  let refreshing: AbortController | undefined;
  let polling: AbortController | undefined;
  let pollTimer: number | undefined;
  let expiryTimer: number | undefined;
  let replayPending = false;
  let latestExpression: TimedExpression | undefined;
  let pendingMessage: { content: string; id: string } | undefined;
  const displayed = new Map<
    string,
    FeedItem & { deadline: number; replay: boolean }
  >();

  function refreshDisplay() {
    window.clearTimeout(expiryTimer);
    const now = performance.now();
    for (const item of displayed.values()) {
      if (item.deadline <= now) displayed.delete(item.id);
    }
    const visible = [...displayed.values()]
      .sort((left, right) => left.createdAt - right.createdAt)
      .slice(-4);
    if (
      visible.length !== bubbles.value.length ||
      visible.some((item, index) => item !== bubbles.value[index])
    )
      bubbles.value = visible;
    const current =
      latestExpression && latestExpression.deadline > now
        ? latestExpression
        : undefined;
    expression.value = current?.id;
    const deadline = Math.min(
      ...[...displayed.values()].map((item) => item.deadline),
      current?.deadline ?? Infinity,
    );
    if (Number.isFinite(deadline))
      expiryTimer = window.setTimeout(
        refreshDisplay,
        Math.max(1, deadline - now),
      );
  }

  function clearDisplay() {
    window.clearTimeout(expiryTimer);
    displayed.clear();
    bubbles.value = [];
    latestExpression = undefined;
    expression.value = undefined;
  }

  function stopPolling() {
    window.clearTimeout(pollTimer);
    polling?.abort();
    polling = undefined;
  }

  function disconnect() {
    requests.abort();
    requests = new AbortController();
    stopPolling();
    clearDisplay();
    session = undefined;
    conversations.value = [];
    selected.value = "";
    draft.value = "";
    pendingMessage = undefined;
    status.value = "signed-out";
    error.value = "";
    feedError.value = "";
  }

  function applyFeed(feed: Feed, replay: boolean) {
    const receivedAt = performance.now();
    const incoming = new Set(feed.items.map(({ id }) => id));
    for (const item of displayed.values()) {
      if (!item.replay && !incoming.has(item.id)) displayed.delete(item.id);
    }
    for (const item of feed.items) {
      const previous = displayed.get(item.id);
      if (previous) {
        if (previous.text !== item.text)
          displayed.set(item.id, { ...previous, text: item.text });
      } else if (item.expiresAt > feed.now) {
        displayed.set(item.id, {
          ...item,
          replay,
          deadline: receivedAt + item.expiresAt - feed.now,
        });
      }
      const action = item.expression;
      if (
        item.role === "assistant" &&
        action &&
        (action.id === "neutral" || allowedExpressions.has(action.id)) &&
        action.expiresAt > feed.now &&
        (!latestExpression ||
          (item.id !== latestExpression.messageId &&
            item.createdAt >= latestExpression.createdAt))
      ) {
        latestExpression = {
          id: action.id,
          createdAt: item.createdAt,
          messageId: item.id,
          deadline: receivedAt + action.expiresAt - feed.now,
        };
      }
    }
    refreshDisplay();
  }

  function schedulePoll(delay: number) {
    window.clearTimeout(pollTimer);
    if (selected.value && session && !document.hidden)
      pollTimer = window.setTimeout(poll, delay);
  }

  async function poll() {
    if (!selected.value || !session || document.hidden) return;
    const controller = new AbortController();
    polling = controller;
    const id = selected.value;
    const replay = replayPending;
    let delay = 1000;
    try {
      const feed = await request<Feed>(
        `/stage/conversations/${encodeURIComponent(id)}/feed${replay ? "?replay=1" : ""}`,
        controller.signal,
      );
      if (controller.signal.aborted) return;
      replayPending = false;
      feedError.value = "";
      applyFeed(feed, replay);
    } catch (cause) {
      if (controller.signal.aborted) return;
      if (cause instanceof StageRequestError && cause.status === 401) {
        disconnect();
      } else if (cause instanceof StageRequestError && cause.status === 404) {
        selectConversation("");
        void refresh();
      } else {
        feedError.value = "对话更新暂时中断，正在重试。";
        delay = 2500;
      }
    } finally {
      if (polling === controller) {
        polling = undefined;
        schedulePoll(delay);
      }
    }
  }

  function selectConversation(id: string) {
    if (id === selected.value) return;
    stopPolling();
    clearDisplay();
    selected.value = id;
    draft.value = "";
    pendingMessage = undefined;
    error.value = "";
    feedError.value = "";
    replayPending = !!id;
    schedulePoll(0);
  }

  async function refresh() {
    refreshing?.abort();
    const controller = new AbortController();
    refreshing = controller;
    const signal = AbortSignal.any([requests.signal, controller.signal]);
    try {
      const next = await request<Session>("/auth/me", signal);
      const result = await request<{ items: Conversation[] }>(
        "/stage/conversations",
        signal,
      );
      if (signal.aborted) return;
      if (session && next.user.id !== session.user.id) {
        requests.abort();
        requests = new AbortController();
        selectConversation("");
      }
      session = next;
      conversations.value = result.items;
      status.value = "signed-in";
      if (
        selected.value &&
        !result.items.some(({ id }) => id === selected.value)
      )
        selectConversation("");
      if (!polling) schedulePoll(0);
    } catch (cause) {
      if (signal.aborted) return;
      if (cause instanceof StageRequestError && cause.status === 401)
        disconnect();
      else if (!session) status.value = "unavailable";
      else error.value = errorText(cause);
    } finally {
      if (refreshing === controller) refreshing = undefined;
    }
  }

  async function mutate(
    operation: (csrf: string, signal: AbortSignal) => Promise<void>,
  ) {
    if (!session || busy.value) return;
    const signal = requests.signal;
    busy.value = true;
    error.value = "";
    try {
      await operation(session.csrf_token, signal);
    } catch (cause) {
      if (signal.aborted) return;
      if (cause instanceof StageRequestError && cause.status === 401)
        disconnect();
      else error.value = errorText(cause);
    } finally {
      busy.value = false;
    }
  }

  function createConversation() {
    return mutate(async (csrf, signal) => {
      const conversation = await request<Conversation>(
        "/conversations",
        signal,
        {
          csrf,
          body: { title: "舞台对话" },
        },
      );
      if (signal.aborted) return;
      conversations.value = [conversation, ...conversations.value];
      selectConversation(conversation.id);
    });
  }

  function sendMessage() {
    const content = draft.value.trim();
    const id = selected.value;
    if (!content || !id) return;
    return mutate(async (csrf, signal) => {
      if (pendingMessage?.content !== content)
        pendingMessage = { content, id: crypto.randomUUID() };
      await request(
        `/conversations/${encodeURIComponent(id)}/messages`,
        signal,
        {
          csrf,
          body: { content, client_message_id: pendingMessage.id },
        },
      );
      if (signal.aborted) return;
      draft.value = "";
      pendingMessage = undefined;
      if (!polling) schedulePoll(0);
    });
  }

  function onVisibility() {
    if (document.hidden) {
      refreshing?.abort();
      stopPolling();
      clearDisplay();
    } else {
      void refresh();
    }
  }

  onMounted(() => {
    document.addEventListener("visibilitychange", onVisibility);
    if (!document.hidden) void refresh();
  });
  onBeforeUnmount(() => {
    document.removeEventListener("visibilitychange", onVisibility);
    requests.abort();
    stopPolling();
    window.clearTimeout(expiryTimer);
  });

  return {
    status,
    conversations,
    selected,
    draft,
    busy,
    error,
    feedError,
    bubbles,
    expression,
    canSend,
    refresh,
    selectConversation,
    createConversation,
    sendMessage,
  };
}
