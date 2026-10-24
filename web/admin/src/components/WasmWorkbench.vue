<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, useId, watch } from "vue";
import { api, ApiError } from "../api";
import { formatDate } from "../validation";
import type { Timestamp } from "../types";

interface Case {
  a: number;
  b: number;
  expected: number;
}
interface TestResult {
  actual?: number | null;
  passed: boolean;
}
type JobStatus =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "expired"
  | "deleted";
interface Job {
  id: string;
  kind: "generate" | "execute";
  status: JobStatus;
  revision: number;
  created_at: Timestamp;
  expires_at: Timestamp;
  error_code?: string | null;
  name?: string | null;
  artifact_id?: string | null;
  result?: number | null;
  test_results?: TestResult[] | null;
}
type ArtifactStatus = "ready" | "approved" | "rejected" | "revoked";
interface Artifact {
  id: string;
  name: string;
  revision: number;
  status: ArtifactStatus;
  wasm_sha256: string;
  tests_sha256: string;
  engine_version: string;
  policy_version: string;
  created_at: Timestamp;
}
interface Detail extends Artifact {
  wat: string;
  tests: Case[];
  test_results: TestResult[];
}
interface View {
  available: boolean;
  generation_available: boolean;
  generation_model: string;
  availability_code?: string | null;
  engine_version: string;
  policy_version: string;
  jobs: Job[];
  artifacts: Artifact[];
  policy: {
    min_cases: number;
    max_cases: number;
    max_spec_bytes: number;
    max_wat_bytes: number;
    max_wasm_bytes: number;
    fuel_per_call: number;
  };
}
interface FormCase {
  key: number;
  a: string;
  b: string;
  expected: string;
}
type ArtifactAction = "approve" | "reject" | "revoke";

const prefix = "/wasm-workbench";
const fieldId = useId();
const view = ref<View | null>(null);
const name = ref("");
const spec = ref("");
let caseKey = 0;
const cases = ref<FormCase[]>([blankCase(), blankCase()]);
const executeA = ref("");
const executeB = ref("");
const selectedArtifact = ref("");
const detail = ref<Detail | null>(null);
const loading = ref(false);
const loadingDetail = ref(false);
const mutating = ref(false);
const conflict = ref(false);
const sessionExpired = ref(false);
const error = ref("");
const notice = ref("");
const confirmation = ref<{ action: ArtifactAction; target: Detail } | null>(
  null,
);
const reviewed = ref(false);
const deleteTarget = ref<Job | null>(null);
const controllers = new Set<AbortController>();
let listController: AbortController | null = null;
let detailController: AbortController | null = null;
let pollController: AbortController | null = null;
let pollTimer: ReturnType<typeof setTimeout> | undefined;
let expiryTimer: ReturnType<typeof setTimeout> | undefined;
let epoch = 0;
let listEpoch = 0;
let detailEpoch = 0;
let pollCount = 0;
let pollIndex = 0;
let disposed = false;
let generationAttempt: {
  id: string;
  fingerprint: string;
  body: { name: string; spec: string; tests: Case[] };
} | null = null;
let executionAttempt: {
  id: string;
  fingerprint: string;
  body: { a: number; b: number; wasm_sha256: string };
} | null = null;

const jobLabels: Record<JobStatus, string> = {
  queued: "等待处理",
  running: "正在处理",
  completed: "任务已完成",
  failed: "任务失败，未自动重试",
  cancelled: "已取消",
  interrupted: "已中断，未自动重试",
  expired: "已到期",
  deleted: "任务已清除",
};
const artifactLabels: Record<ArtifactStatus, string> = {
  ready: "待管理员审核",
  approved: "已按制品批准",
  rejected: "已拒绝",
  revoked: "授权已撤销",
};
const knownErrors: Record<string, string> = {
  fuel_exhausted: "计算燃料耗尽",
  worker_unavailable: "受管计算进程不可用",
  resource_gate_failed: "资源门禁未通过",
  invalid_wat: "WAT 校验未通过",
  invalid_wasm: "WASM 校验未通过",
  test_failed: "测试未通过",
  generation_failed: "生成未完成",
  model_unavailable: "生成模型不可用",
  artifact_revoked: "制品授权已撤销",
  timeout: "任务超时",
  interrupted: "任务已中断",
};
const activeJobs = computed(() =>
  (view.value?.jobs ?? []).filter(
    (item) => item.status === "queued" || item.status === "running",
  ),
);
const busy = computed(
  () =>
    loading.value ||
    loadingDetail.value ||
    mutating.value ||
    sessionExpired.value,
);
const generationLocked = computed(
  () =>
    mutating.value ||
    sessionExpired.value ||
    (conflict.value && !!generationAttempt),
);
const executionLocked = computed(
  () =>
    mutating.value ||
    sessionExpired.value ||
    (conflict.value && !!executionAttempt),
);
const unknownGeneration = computed(
  () => conflict.value && generationAttempt !== null,
);
const unknownExecution = computed(
  () => conflict.value && executionAttempt !== null,
);
const limits = computed(() => ({
  min: Math.max(2, view.value?.policy.min_cases ?? 2),
  max: Math.min(8, view.value?.policy.max_cases ?? 8),
  spec: Math.min(4000, view.value?.policy.max_spec_bytes ?? 4000),
}));
const formError = computed(() => {
  if (!name.value.trim() || bytes(name.value.trim()) > 80)
    return "名称需为 1–80 UTF-8 字节。";
  if (!spec.value.trim() || bytes(spec.value.trim()) > limits.value.spec)
    return `需求需为 1–${limits.value.spec.toLocaleString()} UTF-8 字节。`;
  if (/[\0]/.test(name.value + spec.value))
    return "名称与需求不能包含 NUL 字符。";
  if (
    /(?:\bsk-[A-Za-z0-9_-]{20,}|\bBearer\s+[A-Za-z0-9._-]{16,}|-----BEGIN [A-Z ]*PRIVATE KEY-----)/i.test(
      name.value + "\n" + spec.value,
    )
  )
    return "请移除疑似凭证后再提交需求。";
  if (
    cases.value.length < limits.value.min ||
    cases.value.length > limits.value.max
  )
    return `需要 ${limits.value.min}–${limits.value.max} 组明确测试。`;
  const parsed = cases.value.map(parsedCase);
  if (parsed.some((item) => !item))
    return "每个测试的 a、b 和预期输出都必须是 32 位有符号整数。";
  if (new Set(parsed.map((item) => `${item!.a},${item!.b}`)).size < 2)
    return "至少需要两组不同的 a/b 输入。";
  return "";
});
const canGenerate = computed(
  () =>
    !!view.value?.available &&
    !!view.value.generation_available &&
    !formError.value &&
    !busy.value &&
    !conflict.value &&
    !activeJobs.value.length,
);
const detailSupported = computed(
  () =>
    !!detail.value &&
    detail.value.engine_version === "wasmi-2.0.0" &&
    detail.value.policy_version === "pure-i32-v1" &&
    /^[a-f0-9]{64}$/i.test(detail.value.wasm_sha256) &&
    /^[a-f0-9]{64}$/i.test(detail.value.tests_sha256),
);
const testsVerified = computed(
  () =>
    !!detail.value &&
    detail.value.tests.length >= 2 &&
    detail.value.tests.length <= 8 &&
    detail.value.test_results.length === detail.value.tests.length &&
    detail.value.tests.every(
      (item, index) =>
        detail.value!.test_results[index]?.passed === true &&
        detail.value!.test_results[index]?.actual === item.expected,
    ),
);
const canExecute = computed(
  () =>
    !!view.value?.available &&
    detailSupported.value &&
    testsVerified.value &&
    detail.value?.status === "approved" &&
    i32(executeA.value) !== undefined &&
    i32(executeB.value) !== undefined &&
    !busy.value &&
    !conflict.value &&
    !activeJobs.value.length,
);

function blankCase(): FormCase {
  return { key: ++caseKey, a: "", b: "", expected: "" };
}
function bytes(value: string) {
  return new TextEncoder().encode(value).length;
}
function i32(value: string) {
  if (!/^-?\d+$/.test(value.trim())) return undefined;
  const number = Number(value.trim());
  return Number.isInteger(number) &&
    number >= -2147483648 &&
    number <= 2147483647
    ? number
    : undefined;
}
function parsedCase(item: FormCase): Case | undefined {
  const a = i32(item.a);
  const b = i32(item.b);
  const expected = i32(item.expected);
  return a === undefined || b === undefined || expected === undefined
    ? undefined
    : { a, b, expected };
}
function casePassed(artifact: Detail, index: number) {
  return (
    artifact.test_results[index]?.passed === true &&
    artifact.test_results[index]?.actual === artifact.tests[index]?.expected
  );
}
function timestamp(value: Timestamp) {
  return typeof value === "number"
    ? value < 1e12
      ? value * 1000
      : value
    : Date.parse(value);
}
function live(token: number) {
  return !disposed && !sessionExpired.value && token === epoch;
}
function request() {
  const controller = new AbortController();
  controllers.add(controller);
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, 10000);
  return {
    controller,
    timedOut: () => timedOut,
    done: () => {
      clearTimeout(timer);
      controllers.delete(controller);
    },
  };
}
function stopPolling() {
  clearTimeout(pollTimer);
  pollTimer = undefined;
  pollController?.abort();
  pollController = null;
}
function clearSensitive() {
  epoch++;
  listEpoch++;
  detailEpoch++;
  stopPolling();
  clearTimeout(expiryTimer);
  for (const controller of controllers) controller.abort();
  controllers.clear();
  view.value = null;
  detail.value = null;
  selectedArtifact.value = "";
  name.value = "";
  spec.value = "";
  cases.value = [blankCase(), blankCase()];
  executeA.value = "";
  executeB.value = "";
  generationAttempt = null;
  executionAttempt = null;
  confirmation.value = null;
  deleteTarget.value = null;
  reviewed.value = false;
}
function failure(caught: unknown, fallback: string) {
  stopPolling();
  if (caught instanceof ApiError && caught.status === 401) {
    clearSensitive();
    sessionExpired.value = true;
    loading.value = false;
    loadingDetail.value = false;
    mutating.value = false;
    error.value = "登录已过期，需求、测试、代码详情和结果已清除。请重新登录。";
  } else if (caught instanceof ApiError && caught.status === 409) {
    conflict.value = true;
    confirmation.value = null;
    deleteTarget.value = null;
    reviewed.value = false;
    error.value =
      "任务、制品版本或容量已变化。未自动重试，当前展示已保留；请刷新工坊后重新核对。";
  } else error.value = fallback;
}
function cleanJob(item: Job): Job {
  return item.status === "expired" || item.status === "deleted"
    ? { ...item, name: undefined, result: undefined, test_results: undefined }
    : item.status === "completed"
      ? item
      : { ...item, result: undefined };
}
function upsertJob(item: Job) {
  if (!view.value) return;
  const index = view.value.jobs.findIndex((entry) => entry.id === item.id);
  if (index < 0) view.value.jobs.unshift(cleanJob(item));
  else view.value.jobs[index] = cleanJob(item);
}
function armExpiry() {
  clearTimeout(expiryTimer);
  if (!view.value) return;
  view.value.jobs = view.value.jobs.map((item) =>
    item.status !== "deleted" &&
    item.status !== "expired" &&
    timestamp(item.expires_at) <= Date.now()
      ? {
          ...item,
          status: "expired",
          name: undefined,
          result: undefined,
          test_results: undefined,
        }
      : item,
  );
  if (
    deleteTarget.value &&
    timestamp(deleteTarget.value.expires_at) <= Date.now()
  )
    deleteTarget.value = null;
  const next = Math.min(
    ...view.value.jobs
      .filter((item) => item.status !== "deleted" && item.status !== "expired")
      .map((item) => timestamp(item.expires_at))
      .filter((value) => Number.isFinite(value) && value > Date.now()),
  );
  if (Number.isFinite(next))
    expiryTimer = setTimeout(
      () => {
        armExpiry();
        if (!activeJobs.value.length) stopPolling();
      },
      Math.min(2147483647, Math.max(1, next - Date.now())),
    );
}
function schedulePoll() {
  clearTimeout(pollTimer);
  if (
    disposed ||
    sessionExpired.value ||
    conflict.value ||
    !activeJobs.value.length
  )
    return;
  if (pollCount >= 60) {
    error.value =
      "状态查询已达到本次上限，轮询已暂停。请手动刷新工坊确认结果。";
    return;
  }
  pollTimer = setTimeout(() => void poll(), 2000);
}
async function loadDetail() {
  const id = selectedArtifact.value;
  if (!id || disposed || sessionExpired.value) return;
  detailController?.abort();
  const token = epoch;
  const read = ++detailEpoch;
  const pending = request();
  detailController = pending.controller;
  loadingDetail.value = true;
  detail.value = null;
  confirmation.value = null;
  reviewed.value = false;
  try {
    const result = await api<Detail>(
      `${prefix}/artifacts/${encodeURIComponent(id)}`,
      { signal: pending.controller.signal },
    );
    if (
      !live(token) ||
      read !== detailEpoch ||
      selectedArtifact.value !== id ||
      pending.controller.signal.aborted
    )
      return;
    if (
      result.id !== id ||
      typeof result.wat !== "string" ||
      bytes(result.wat) > 65536 ||
      !Array.isArray(result.tests) ||
      !Array.isArray(result.test_results)
    )
      throw new Error("invalid artifact detail");
    detail.value = result;
  } catch (caught) {
    if (
      live(token) &&
      read === detailEpoch &&
      (!pending.controller.signal.aborted || pending.timedOut())
    )
      failure(caught, "制品详情未能读取，审批与执行保持关闭。请刷新后重试。");
  } finally {
    pending.done();
    if (live(token) && read === detailEpoch) loadingDetail.value = false;
  }
}
async function refresh(resetPolling = true) {
  if (disposed || sessionExpired.value) return;
  stopPolling();
  listController?.abort();
  detailController?.abort();
  detailEpoch++;
  detail.value = null;
  confirmation.value = null;
  reviewed.value = false;
  const token = epoch;
  const read = ++listEpoch;
  const pending = request();
  listController = pending.controller;
  loading.value = true;
  error.value = "";
  try {
    const result = await api<View>(prefix, {
      signal: pending.controller.signal,
    });
    if (!live(token) || read !== listEpoch || pending.controller.signal.aborted)
      return;
    result.jobs = result.jobs.map(cleanJob);
    view.value = result;
    conflict.value = false;
    if (resetPolling) {
      pollCount = 0;
      pollIndex = 0;
    }
    armExpiry();
    if (
      selectedArtifact.value &&
      result.artifacts.some((item) => item.id === selectedArtifact.value)
    )
      await loadDetail();
    else selectedArtifact.value = "";
    if (!mutating.value) schedulePoll();
  } catch (caught) {
    if (
      live(token) &&
      read === listEpoch &&
      (!pending.controller.signal.aborted || pending.timedOut())
    )
      failure(
        caught,
        "工坊状态未能更新，旧列表已保留；请手动刷新。服务端详细错误不会在此回显。",
      );
  } finally {
    pending.done();
    if (live(token) && read === listEpoch) loading.value = false;
  }
}
async function poll() {
  pollTimer = undefined;
  if (
    disposed ||
    sessionExpired.value ||
    conflict.value ||
    !activeJobs.value.length
  )
    return;
  if (busy.value) {
    schedulePoll();
    return;
  }
  const target = activeJobs.value[pollIndex++ % activeJobs.value.length]!;
  const token = epoch;
  const pending = request();
  pollController = pending.controller;
  pollCount++;
  try {
    const result = await api<Job>(
      `${prefix}/jobs/${encodeURIComponent(target.id)}`,
      { signal: pending.controller.signal },
    );
    if (!live(token) || pending.controller.signal.aborted) return;
    if (result.id !== target.id) throw new Error("mismatched job");
    upsertJob(result);
    armExpiry();
    if (result.status !== "queued" && result.status !== "running")
      await refresh(false);
    else schedulePoll();
  } catch (caught) {
    if (
      live(token) &&
      (!pending.controller.signal.aborted || pending.timedOut())
    )
      failure(caught, "任务状态查询已停止。请手动刷新工坊，不会自动重试计算。");
  } finally {
    pending.done();
    if (pollController === pending.controller) pollController = null;
  }
}
async function mutate(action: (signal: AbortSignal) => Promise<void>) {
  if (busy.value || conflict.value) return;
  const token = epoch;
  const pending = request();
  stopPolling();
  mutating.value = true;
  error.value = "";
  notice.value = "";
  try {
    await action(pending.controller.signal);
  } catch (caught) {
    if (live(token)) {
      if (!(caught instanceof ApiError && caught.status === 401))
        conflict.value = true;
      failure(
        caught,
        "未能确认操作结果，未自动重发。请刷新工坊核对；详细服务错误不会回显。",
      );
    }
  } finally {
    pending.done();
    if (live(token)) {
      mutating.value = false;
      if (!conflict.value) schedulePoll();
    }
  }
}
async function generate() {
  if (!canGenerate.value) return;
  const body = {
    name: name.value.trim(),
    spec: spec.value.trim(),
    tests: cases.value.map((item) => parsedCase(item)!),
  };
  const fingerprint = JSON.stringify(body);
  if (!generationAttempt || generationAttempt.fingerprint !== fingerprint)
    generationAttempt = { id: crypto.randomUUID(), fingerprint, body };
  const attempt = generationAttempt;
  const token = epoch;
  await mutate(async (signal) => {
    const result = await api<Job>(`${prefix}/generate`, {
      method: "POST",
      body: { client_request_id: attempt.id, ...attempt.body },
      signal,
    });
    if (!live(token) || signal.aborted) return;
    upsertJob(result);
    generationAttempt = null;
    pollCount = 0;
    notice.value =
      "已提交一次生成。模型代码及自测不等于正确；制品仍需人工核对并批准，未自动执行。";
    armExpiry();
    if (result.status !== "queued" && result.status !== "running")
      await refresh();
  });
}
function askAction(action: ArtifactAction) {
  if (!detail.value || busy.value || conflict.value) return;
  if (
    action === "approve" &&
    (detail.value.status !== "ready" ||
      !detailSupported.value ||
      !testsVerified.value)
  )
    return;
  if (action === "revoke" && detail.value.status !== "approved") return;
  confirmation.value = { action, target: detail.value };
  reviewed.value = false;
}
async function changeArtifact() {
  const target = confirmation.value;
  if (!target || (target.action === "approve" && !reviewed.value)) return;
  const token = epoch;
  await mutate(async (signal) => {
    await api<unknown>(
      `${prefix}/artifacts/${encodeURIComponent(target.target.id)}/${target.action}`,
      {
        method: "POST",
        body:
          target.action === "approve"
            ? {
                wasm_sha256: target.target.wasm_sha256,
                tests_sha256: target.target.tests_sha256,
              }
            : {},
        revision: target.target.revision,
        signal,
      },
    );
    if (!live(token) || signal.aborted) return;
    const next: ArtifactStatus =
      target.action === "approve"
        ? "approved"
        : target.action === "reject"
          ? "rejected"
          : "revoked";
    if (view.value) {
      view.value.artifacts = view.value.artifacts.map((item) =>
        item.id === target.target.id ? { ...item, status: next } : item,
      );
      if (target.action === "revoke")
        view.value.jobs = view.value.jobs.map((item) =>
          item.artifact_id === target.target.id &&
          (item.status === "queued" || item.status === "running")
            ? { ...item, status: "interrupted", result: undefined }
            : item,
        );
    }
    detail.value = null;
    confirmation.value = null;
    reviewed.value = false;
    executionAttempt = null;
    notice.value =
      target.action === "approve"
        ? "制品批准已保存。重新读取确认版本前，手动执行保持关闭；未自动执行。"
        : target.action === "reject"
          ? "制品已拒绝，未授予执行权限；私有代码继续保留。"
          : "制品授权已撤销，相关排队或运行结果已中断；私有代码继续保留。";
    await refresh();
  });
}
async function execute() {
  const artifact = detail.value;
  if (!artifact || !canExecute.value) return;
  const body = {
    a: i32(executeA.value)!,
    b: i32(executeB.value)!,
    wasm_sha256: artifact.wasm_sha256,
  };
  const fingerprint = JSON.stringify({
    artifact_id: artifact.id,
    revision: artifact.revision,
    ...body,
  });
  if (!executionAttempt || executionAttempt.fingerprint !== fingerprint)
    executionAttempt = { id: crypto.randomUUID(), fingerprint, body };
  const attempt = executionAttempt;
  const token = epoch;
  await mutate(async (signal) => {
    const result = await api<Job>(
      `${prefix}/artifacts/${encodeURIComponent(artifact.id)}/execute`,
      {
        method: "POST",
        body: { client_request_id: attempt.id, ...attempt.body },
        revision: artifact.revision,
        signal,
      },
    );
    if (!live(token) || signal.aborted) return;
    upsertJob(result);
    executionAttempt = null;
    pollCount = 0;
    notice.value =
      "已提交一次手动计算，未注册聊天工具，也未授予文件、网络或其他宿主权限。";
    armExpiry();
    if (result.status !== "queued" && result.status !== "running")
      await refresh();
  });
}
async function removeJob() {
  const target = deleteTarget.value;
  if (!target) return;
  const token = epoch;
  await mutate(async (signal) => {
    await api<unknown>(`${prefix}/jobs/${encodeURIComponent(target.id)}`, {
      method: "DELETE",
      revision: target.revision,
      signal,
    });
    if (!live(token) || signal.aborted) return;
    upsertJob({
      ...target,
      status: "deleted",
      name: undefined,
      result: undefined,
      test_results: undefined,
    });
    deleteTarget.value = null;
    notice.value = "本任务已取消或清除；制品授权与私有代码保留策略独立。";
    await refresh();
  });
}
watch(selectedArtifact, () => {
  detailEpoch++;
  detailController?.abort();
  detail.value = null;
  confirmation.value = null;
  reviewed.value = false;
  loadingDetail.value = false;
  executeA.value = "";
  executeB.value = "";
  executionAttempt = null;
  if (selectedArtifact.value && !sessionExpired.value && !disposed)
    void loadDetail();
});
onMounted(() => void refresh());
onUnmounted(() => {
  disposed = true;
  clearSensitive();
});
</script>

<template>
  <section class="panel wasm-panel" aria-label="纯整数计算工坊">
    <div class="panel-heading wasm-heading">
      <div>
        <h2>计算工坊</h2>
        <p>独立的纯整数 WASM，不注册聊天工具，不授予宿主权限。</p>
      </div>
      <button
        type="button"
        class="button secondary small"
        :disabled="busy"
        @click="refresh()"
      >
        刷新工坊
      </button>
    </div>
    <p class="wasm-policy">
      只支持 calc(a, b) → i32，输入输出均为 32 位有符号整数；溢出按 WASM
      整数语义回绕。禁止文件、网络、导入、内存、table、global 和
      start。执行需经过解释器与受管子进程资源门禁，模型生成代码和自测不等于正确，审批前不得手动执行。
    </p>
    <div v-if="error" class="alert error" role="alert">
      <span>{{ error }}</span>
    </div>
    <div v-if="notice" class="alert success" role="status">
      <span>{{ notice }}</span>
    </div>
    <p v-if="loading && !view" class="footnote" role="status">
      正在读取工坊门禁与私有任务…
    </p>
    <template v-if="view">
      <p class="footnote">
        引擎 {{ view.engine_version }} · 策略 {{ view.policy_version }} ·
        每次调用燃料上限 {{ view.policy.fuel_per_call.toLocaleString() }}
      </p>
      <p v-if="!view.available" class="wasm-unavailable" role="status">
        当前主机的受管计算进程资源门禁尚不可用，生成与执行保持关闭。界面和模拟测试不代表真实执行已启用。
      </p>
      <p v-else-if="!view.generation_available" class="footnote">
        当前可以管理已批准制品；生成模型凭证尚不可用，不能提交新的模型生成。
      </p>
      <form
        class="wasm-generate"
        aria-label="生成纯整数制品"
        @submit.prevent="generate"
      >
        <h3>1. 明确需求与测试</h3>
        <p class="footnote">
          点击“发送需求并生成”才会把本表单需求与测试发送至已配置模型
          {{
            view.generation_model || "（尚未提供名称）"
          }}。不要填写凭证或其他秘密，不自动重发。
        </p>
        <fieldset :disabled="generationLocked">
          <legend>生成输入</legend>
          <label class="field"
            ><span>制品名称</span
            ><input
              v-model="name"
              aria-label="制品名称"
              :aria-describedby="`${fieldId}-name-help`"
            /><small :id="`${fieldId}-name-help`"
              >{{ bytes(name).toLocaleString() }} / 80 UTF-8 字节</small
            ></label
          >
          <label class="field"
            ><span>计算需求</span
            ><textarea
              v-model="spec"
              aria-label="计算需求"
              :aria-describedby="`${fieldId}-spec-help`"
              rows="3"
            ></textarea
            ><small :id="`${fieldId}-spec-help`"
              >{{ bytes(spec).toLocaleString() }} /
              {{ limits.spec.toLocaleString() }} UTF-8
              字节；只描述纯整数计算。</small
            ></label
          >
          <div v-for="(item, index) in cases" :key="item.key" class="wasm-case">
            <strong>测试 {{ index + 1 }}</strong>
            <div class="wasm-case-inputs">
              <label class="field"
                ><span>输入 a</span
                ><input
                  v-model="item.a"
                  inputmode="numeric"
                  :aria-label="`测试 ${index + 1} 输入 a`" /></label
              ><label class="field"
                ><span>输入 b</span
                ><input
                  v-model="item.b"
                  inputmode="numeric"
                  :aria-label="`测试 ${index + 1} 输入 b`" /></label
              ><label class="field"
                ><span>预期输出</span
                ><input
                  v-model="item.expected"
                  inputmode="numeric"
                  :aria-label="`测试 ${index + 1} 预期输出`"
              /></label>
            </div>
            <button
              type="button"
              class="button secondary small"
              :aria-label="`移除测试 ${index + 1}`"
              :disabled="cases.length <= limits.min"
              @click="cases.splice(index, 1)"
            >
              移除此测试
            </button>
          </div>
          <button
            type="button"
            class="button secondary small"
            :disabled="cases.length >= limits.max"
            @click="cases.push(blankCase())"
          >
            添加测试
          </button>
        </fieldset>
        <p v-if="formError && (name || spec)" class="footnote" role="alert">
          {{ formError }}
        </p>
        <p class="footnote">
          需要 {{ limits.min }}–{{ limits.max }} 组测试，至少两组不同的
          a/b；允许整数范围 -2147483648 到 2147483647。
        </p>
        <p v-if="unknownGeneration" class="footnote">
          未确认请求的输入已暂时锁定。先刷新；原样重试沿用同一请求，修改输入后再次明确发送才建立新请求。
        </p>
        <button type="submit" class="button primary" :disabled="!canGenerate">
          发送需求并生成
        </button>
      </form>
      <section class="wasm-artifacts" aria-label="私有制品">
        <h3>2. 查看并按制品批准</h3>
        <p class="footnote">
          只列出当前已认证账户的制品。选择详情只读取代码，既不批准，也不执行。
        </p>
        <p v-if="!view.artifacts.length" class="footnote">
          暂无私有制品。模型生成成功后仍需核对代码与真实测试结果。
        </p>
        <div class="wasm-artifact-list">
          <button
            v-for="item in view.artifacts"
            :key="item.id"
            type="button"
            class="wasm-artifact-button"
            :aria-label="`查看制品 ${item.name}`"
            :aria-pressed="selectedArtifact === item.id"
            :disabled="mutating || sessionExpired"
            @click="selectedArtifact = item.id"
          >
            <strong>{{ item.name }}</strong
            ><span>{{ artifactLabels[item.status] || "状态待确认" }}</span>
          </button>
        </div>
        <p v-if="loadingDetail" class="footnote" role="status">
          正在读取所选制品详情…
        </p>
        <article v-if="detail" class="wasm-detail" aria-label="所选制品详情">
          <h3>{{ detail.name }}</h3>
          <p class="footnote">
            {{ artifactLabels[detail.status] || "状态待确认" }} ·
            {{ detail.engine_version }} · {{ detail.policy_version }}
          </p>
          <dl class="wasm-hashes">
            <div>
              <dt>WASM SHA-256</dt>
              <dd>
                <code>{{ detail.wasm_sha256 }}</code>
              </dd>
            </div>
            <div>
              <dt>测试 SHA-256</dt>
              <dd>
                <code>{{ detail.tests_sha256 }}</code>
              </dd>
            </div>
          </dl>
          <h4>WAT 源码（只读）</h4>
          <pre class="wasm-source" tabindex="0" aria-label="WAT 源码">{{
            detail.wat
          }}</pre>
          <h4>测试与实际结果</h4>
          <ol class="wasm-test-results">
            <li v-for="(item, index) in detail.tests" :key="index">
              <div>
                <span>a：{{ item.a }}</span
                ><span>b：{{ item.b }}</span
                ><span>预期：{{ item.expected }}</span
                ><span
                  >实际：{{
                    detail.test_results[index]?.actual === undefined ||
                    detail.test_results[index]?.actual === null
                      ? "无返回值"
                      : detail.test_results[index]?.actual
                  }}</span
                >
              </div>
              <strong>{{
                casePassed(detail, index) ? "测试通过" : "未通过或尚未验证"
              }}</strong>
            </li>
          </ol>
          <p v-if="!detailSupported" class="footnote">
            制品的
            hash、引擎或策略版本尚不满足此页面审核要求，审批与执行保持关闭。
          </p>
          <p v-if="!testsVerified" class="footnote">
            测试实际值尚未逐项匹配预期，批准与手动执行保持关闭。
          </p>
          <div class="wasm-actions">
            <button
              v-if="detail.status === 'ready'"
              type="button"
              class="button primary small"
              :disabled="busy || conflict || !detailSupported || !testsVerified"
              @click="askAction('approve')"
            >
              核对并批准此制品</button
            ><button
              v-if="detail.status === 'ready'"
              type="button"
              class="button secondary small"
              :disabled="busy || conflict"
              @click="askAction('reject')"
            >
              拒绝此制品</button
            ><button
              v-if="detail.status === 'approved'"
              type="button"
              class="button secondary small"
              :disabled="busy || conflict"
              @click="askAction('revoke')"
            >
              撤销此制品授权
            </button>
          </div>
          <form
            class="wasm-execute"
            aria-label="手动执行制品"
            @submit.prevent="execute"
          >
            <h3>3. 手动执行</h3>
            <p class="footnote">
              只有当前版本已批准且资源门禁可用时才能手动执行；批准后不会自动运行。不接入普通聊天或记忆。
            </p>
            <fieldset :disabled="executionLocked">
              <legend>本次整数输入</legend>
              <div class="wasm-execute-inputs">
                <label class="field"
                  ><span>输入 a</span
                  ><input
                    v-model="executeA"
                    inputmode="numeric"
                    aria-label="手动执行输入 a" /></label
                ><label class="field"
                  ><span>输入 b</span
                  ><input
                    v-model="executeB"
                    inputmode="numeric"
                    aria-label="手动执行输入 b"
                /></label>
              </div>
            </fieldset>
            <p v-if="unknownExecution" class="footnote">
              先刷新以核对未确认计算。相同制品版本和输入重试沿用原请求，修改后再次点击才建立新请求。
            </p>
            <button
              type="submit"
              class="button primary small"
              :disabled="!canExecute"
            >
              手动执行已批准制品
            </button>
          </form>
        </article>
      </section>
      <section
        v-if="confirmation"
        class="wasm-confirm"
        aria-label="确认制品授权操作"
      >
        <h3>
          {{
            confirmation.action === "approve"
              ? "按以下制品和测试批准？"
              : confirmation.action === "reject"
                ? "拒绝这个制品？"
                : "撤销这个制品的授权？"
          }}
        </h3>
        <strong>{{ confirmation.target.name }}</strong>
        <p class="footnote">
          {{ confirmation.target.engine_version }} ·
          {{ confirmation.target.policy_version }}
        </p>
        <dl class="wasm-hashes">
          <div>
            <dt>WASM SHA-256</dt>
            <dd>
              <code>{{ confirmation.target.wasm_sha256 }}</code>
            </dd>
          </div>
          <div>
            <dt>测试 SHA-256</dt>
            <dd>
              <code>{{ confirmation.target.tests_sha256 }}</code>
            </dd>
          </div>
        </dl>
        <template v-if="confirmation.action === 'approve'">
          <pre class="wasm-source" tabindex="0" aria-label="待批准 WAT 源码">{{
            confirmation.target.wat
          }}</pre>
          <ol class="wasm-test-results">
            <li v-for="(item, index) in confirmation.target.tests" :key="index">
              a={{ item.a }}，b={{ item.b }}，预期={{ item.expected }}；实际={{
                confirmation.target.test_results[index]?.actual === undefined ||
                confirmation.target.test_results[index]?.actual === null
                  ? "无返回值"
                  : confirmation.target.test_results[index]?.actual
              }}；{{
                casePassed(confirmation.target, index)
                  ? "通过"
                  : "未通过或未验证"
              }}
            </li>
          </ol>
          <label class="wasm-reviewed"
            ><input
              v-model="reviewed"
              type="checkbox"
              aria-label="已核对源码测试及两个制品哈希"
              :disabled="mutating"
            /><span
              >我已核对源码、测试、两个完整 hash
              及引擎/策略；模型自测不保证正确。本次仅批准此制品，未授予宿主权限。</span
            ></label
          ></template
        >
        <p v-else-if="confirmation.action === 'revoke'">
          撤销将停止授权并中断该制品排队或运行中的计算结果；私有 WAT
          与测试继续保留，不等于删除代码。
        </p>
        <p v-else>拒绝此制品不会授予执行权限，私有 WAT 与测试继续保留。</p>
        <div class="wasm-actions">
          <button
            type="button"
            class="button primary small"
            :disabled="
              busy ||
              conflict ||
              (confirmation.action === 'approve' && !reviewed)
            "
            @click="changeArtifact"
          >
            {{
              confirmation.action === "approve"
                ? "确认按两个哈希批准"
                : confirmation.action === "reject"
                  ? "确认拒绝此制品"
                  : "确认撤销并中断相关计算"
            }}</button
          ><button
            type="button"
            class="button secondary small"
            :disabled="mutating"
            @click="
              confirmation = null;
              reviewed = false;
            "
          >
            返回核对
          </button>
        </div>
      </section>
      <section class="wasm-jobs" aria-label="工坊任务">
        <h3>任务与手动结果</h3>
        <p v-if="!view.jobs.length" class="footnote">
          暂无任务。填写表单或查看制品都不会自动发起模型生成或计算。
        </p>
        <article
          v-for="(item, index) in view.jobs"
          :key="item.id"
          class="wasm-job"
          :aria-label="`工坊任务 ${index + 1}`"
        >
          <div class="wasm-job-heading">
            <strong
              >{{ item.kind === "generate" ? "模型生成" : "手动计算" }} ·
              {{ jobLabels[item.status] || "状态待确认" }}</strong
            ><span v-if="item.name">{{ item.name }}</span>
          </div>
          <p class="footnote">
            创建 {{ formatDate(item.created_at) }} · 到期
            {{ formatDate(item.expires_at) }}
          </p>
          <p
            v-if="
              item.status === 'completed' &&
              item.result !== undefined &&
              item.result !== null
            "
            class="wasm-integer-result"
            aria-label="整数计算结果"
          >
            结果：{{ item.result }}
          </p>
          <ul v-if="item.test_results?.length" class="wasm-test-results">
            <li
              v-for="(result, resultIndex) in item.test_results"
              :key="resultIndex"
            >
              测试 {{ resultIndex + 1 }}：{{
                result.passed ? "通过" : "未通过"
              }}；实际
              {{
                result.actual === undefined || result.actual === null
                  ? "无返回值"
                  : result.actual
              }}
            </li>
          </ul>
          <p v-if="item.error_code" class="footnote">
            {{
              knownErrors[item.error_code] ||
              "任务错误已记录，详细内部信息不在页面公开。"
            }}
          </p>
          <button
            v-if="item.status !== 'deleted' && item.status !== 'expired'"
            type="button"
            class="button secondary small"
            :disabled="busy || conflict"
            @click="deleteTarget = item"
          >
            {{
              item.status === "queued" || item.status === "running"
                ? "取消并清除此任务"
                : "清除此任务"
            }}
          </button>
        </article>
      </section>
      <section
        v-if="deleteTarget"
        class="wasm-confirm"
        aria-label="确认清除工坊任务"
      >
        <h3>
          取消或清除这次{{
            deleteTarget.kind === "generate" ? "生成" : "手动计算"
          }}任务？
        </h3>
        <p>
          {{ deleteTarget.name || "未命名任务" }}，创建
          {{
            formatDate(deleteTarget.created_at)
          }}。不会自动重试；此操作不替代制品撤销，也不宣称删除所有备份。
        </p>
        <div class="wasm-actions">
          <button
            type="button"
            class="button primary small"
            :disabled="busy || conflict"
            @click="removeJob"
          >
            确认取消或清除本任务</button
          ><button
            type="button"
            class="button secondary small"
            :disabled="mutating"
            @click="deleteTarget = null"
          >
            继续查看
          </button>
        </div>
      </section>
    </template>
  </section>
</template>

<style scoped>
.wasm-panel,
.wasm-panel > *,
.wasm-panel .field,
.wasm-panel fieldset {
  min-width: 0;
}
.wasm-heading {
  flex-wrap: wrap;
}
.wasm-heading > div {
  min-width: 0;
  flex: 1 1 220px;
}
.wasm-policy,
.wasm-unavailable {
  padding: 12px 14px;
  border-radius: 8px;
  background: var(--soft-blue);
  color: var(--muted);
  font-size: 12px;
  line-height: 1.9;
  margin-bottom: 18px;
}
.wasm-panel input,
.wasm-panel textarea {
  min-width: 0;
  width: 100%;
  max-width: 100%;
}
.wasm-panel button {
  white-space: normal;
  max-width: 100%;
  text-align: left;
}
.wasm-panel h3 {
  font-size: 14px;
  line-height: 1.8;
  margin: 18px 0 10px;
  overflow-wrap: anywhere;
}
.wasm-panel h4 {
  font-size: 12px;
  margin: 16px 0 8px;
}
.wasm-panel fieldset {
  border: 0;
  margin: 14px 0;
  padding: 0;
}
.wasm-panel legend {
  font-size: 12px;
  font-weight: 600;
  margin-bottom: 12px;
}
.wasm-case,
.wasm-detail,
.wasm-job {
  padding: 14px;
  border: 1px solid var(--line);
  border-radius: 9px;
  margin-bottom: 12px;
  min-width: 0;
}
.wasm-case > strong {
  font-size: 12px;
}
.wasm-case-inputs {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 10px;
  margin-top: 12px;
}
.wasm-execute-inputs {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
}
.wasm-artifacts,
.wasm-jobs {
  margin-top: 24px;
}
.wasm-artifact-list {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  margin: 14px 0;
}
.wasm-artifact-button {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 6px;
  max-width: 100%;
  padding: 12px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: white;
  color: var(--ink);
  cursor: pointer;
  overflow-wrap: anywhere;
}
.wasm-artifact-button[aria-pressed="true"] {
  border-color: var(--indigo);
  background: var(--soft-blue);
}
.wasm-artifact-button span {
  font-size: 11px;
  color: var(--muted);
}
.wasm-source {
  max-height: 320px;
  overflow: auto;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  word-break: break-word;
  padding: 12px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--soft-blue);
  font-size: 12px;
  line-height: 1.8;
  min-width: 0;
}
.wasm-hashes {
  font-size: 12px;
  margin: 14px 0;
}
.wasm-hashes > div {
  margin: 10px 0;
}
.wasm-hashes dt {
  font-weight: 600;
}
.wasm-hashes dd {
  margin: 4px 0 0;
  min-width: 0;
}
.wasm-hashes code {
  overflow-wrap: anywhere;
  word-break: break-all;
}
.wasm-test-results {
  margin: 12px 0;
  padding-left: 20px;
  font-size: 12px;
  line-height: 1.9;
  overflow-wrap: anywhere;
}
.wasm-test-results li {
  margin-bottom: 10px;
}
.wasm-test-results li > div {
  display: flex;
  flex-wrap: wrap;
  gap: 5px 14px;
}
.wasm-confirm {
  padding: 16px;
  border: 1px solid #e4d2b1;
  border-radius: 9px;
  background: #fdf9f1;
  margin: 18px 0;
  min-width: 0;
}
.wasm-confirm > p {
  font-size: 12px;
  line-height: 1.9;
  margin: 12px 0;
}
.wasm-confirm > strong {
  overflow-wrap: anywhere;
}
.wasm-reviewed {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  font-size: 12px;
  line-height: 1.9;
}
.wasm-reviewed input {
  width: 16px;
  height: 16px;
  flex: 0 0 auto;
  margin-top: 3px;
}
.wasm-reviewed span {
  min-width: 0;
}
.wasm-actions,
.wasm-job-heading {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  align-items: center;
  margin-top: 14px;
}
.wasm-job-heading {
  font-size: 13px;
  overflow-wrap: anywhere;
}
.wasm-job > .footnote {
  margin: 12px 0;
}
.wasm-integer-result {
  font-size: 16px;
  font-weight: 600;
  margin: 14px 0;
}
@media (max-width: 480px) {
  .wasm-case,
  .wasm-detail,
  .wasm-job,
  .wasm-confirm {
    padding: 12px;
  }
  .wasm-case-inputs {
    grid-template-columns: 1fr;
    gap: 0;
  }
  .wasm-actions {
    flex-direction: column;
    align-items: stretch;
  }
}
</style>
