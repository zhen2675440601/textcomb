<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { api, ApiProblem } from "@/api/client";
import type { Analysis } from "@/api/types";
import { createTaskMonitor, type ConnectionState } from "@/utils/task-monitor";

const route = useRoute();
const router = useRouter();
const job = ref<Analysis | null>(null);
const error = ref("");
const acting = ref(false);
const connection = ref<ConnectionState>("connecting");
const updatedAt = ref<number | null>(null);
const now = ref(Date.now());
let monitor: ReturnType<typeof createTaskMonitor<Analysis>> | undefined;
let clock: ReturnType<typeof setInterval> | undefined;

const jobId = computed(() => String(route.params.id));
const terminal = computed(() =>
  job.value
    ? ["completed", "failed", "cancelled", "expired"].includes(job.value.status)
    : false,
);

const stages = [
  { key: "extracting", label: "提取正文", threshold: 8 },
  { key: "analyzing", label: "发现候选", threshold: 20 },
  { key: "verifying", label: "二轮复核", threshold: 55 },
  { key: "merging", label: "定位去重", threshold: 88 },
  { key: "rendering", label: "生成报告", threshold: 94 },
];

function stageState(threshold: number) {
  if (!job.value) return "pending";
  if (job.value.status === "completed") return "done";
  const currentIndex = stages.findIndex((stage) => stage.key === job.value?.stage);
  const thisIndex = stages.findIndex((stage) => stage.threshold === threshold);
  if (thisIndex < currentIndex) return "done";
  return currentIndex === thisIndex ? "active" : "pending";
}

const elapsed = computed(() => {
  if (!job.value) return "—";
  const end = job.value.completed_at ? Date.parse(job.value.completed_at) : now.value;
  const seconds = Math.max(0, Math.floor((end - Date.parse(job.value.created_at)) / 1000));
  return seconds >= 60 ? `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒` : `${seconds} 秒`;
});
const lastUpdate = computed(() => updatedAt.value === null ? "等待更新" : `${Math.max(0, Math.floor((now.value - updatedAt.value) / 1000))} 秒前更新`);

const statusCopy = computed(() => {
  if (!job.value) return "正在读取任务";
  const labels: Record<string, string> = {
    queued: "任务已进入队列",
    extracting: "正在提取文章正文",
    analyzing: "正在寻找可能的问题",
    verifying: "正在复核候选问题",
    merging: "正在定位并合并重复项",
    rendering: "正在生成三种报告",
    completed: "分析已经完成",
    failed: "分析未能完成",
    cancel_requested: "正在安全取消任务",
    cancelled: "任务已取消",
    expired: "任务已过期",
  };
  return labels[job.value.status] ?? job.value.stage;
});

function connect() {
  monitor?.stop();
  const id = jobId.value;
  monitor = createTaskMonitor<Analysis>({
    fetch: (signal) => api.analysis(id, signal),
    subscribe: (receive, failed, opened) => {
      const events = new EventSource(`/api/v1/analyses/${id}/events`, { withCredentials: true });
      events.onopen = opened;
      events.onerror = failed;
      events.addEventListener("progress", (event) => {
        try { receive(JSON.parse((event as MessageEvent).data) as Analysis); }
        catch { failed(); }
      });
      return events;
    },
    terminal: (value) => ["completed", "failed", "cancelled", "expired"].includes(value.status),
    update: (value) => {
      job.value = value;
      updatedAt.value = Date.now();
      error.value = "";
      if (value.status === "completed" && value.report_id) void router.replace(`/reports/${value.report_id}`);
    },
    connection: (value) => { connection.value = value; },
    error: (cause) => {
      error.value = cause instanceof ApiProblem ? cause.message : "连接暂时中断，正在自动恢复";
      return !(cause instanceof ApiProblem && [401, 403, 404].includes(cause.status));
    },
  });
  void monitor.refresh();
}

async function cancel() {
  if (!window.confirm("确定取消这次分析吗？已产生的模型调用无法撤回。")) return;
  acting.value = true;
  try {
    await api.cancelAnalysis(jobId.value);
    await monitor?.refresh();
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "取消失败";
  } finally {
    acting.value = false;
  }
}

async function retry() {
  acting.value = true;
  error.value = "";
  try {
    job.value = await api.retryAnalysis(jobId.value);
    connect();
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "重试失败";
  } finally {
    acting.value = false;
  }
}

watch(jobId, () => { job.value = null; error.value = ""; updatedAt.value = null; connect(); }, { immediate: true });
function online() { if (!terminal.value) void monitor?.refresh(); }
onMounted(() => {
  clock = setInterval(() => { now.value = Date.now(); }, 1_000);
  window.addEventListener("online", online);
});
onBeforeUnmount(() => { monitor?.stop(); clearInterval(clock); window.removeEventListener("online", online); });
</script>

<template>
  <section class="progress-page">
    <div v-if="error" class="alert error" role="alert">{{ error }} <button v-if="connection === 'stopped'" class="text-button" type="button" @click="connect">重新连接</button></div>
    <div v-if="connection === 'reconnecting'" class="alert" role="status">连接中断，正在自动重连</div>
    <div v-if="!job && !error" class="card loading-card">
      <span class="spinner" /> 正在读取任务…
    </div>

    <template v-else-if="job">
      <div class="progress-hero card" :class="{ failed: job.status === 'failed' }">
        <div class="progress-orbit">
          <svg viewBox="0 0 120 120" aria-hidden="true">
            <circle class="orbit-bg" cx="60" cy="60" r="52" />
            <circle
              class="orbit-value"
              cx="60"
              cy="60"
              r="52"
              :style="{ '--progress': job.progress }"
            />
          </svg>
          <div>
            <strong>{{ job.progress }}</strong>
            <span>%</span>
          </div>
        </div>
        <div class="progress-copy">
          <h2>{{ statusCopy }}</h2>
          <p class="progress-timing">已耗时 {{ elapsed }}（含排队） · {{ lastUpdate }}</p>
          <p v-if="job.status === 'failed'" class="failure-message">
            {{ job.error_message ?? "模型服务或文档处理发生错误。" }}
          </p>
          <p v-else>
            已完成 {{ job.completed_chunks }} / {{ job.total_chunks || "—" }} 个文本块。
          </p>
          <div class="hero-actions">
            <button
              v-if="!terminal && job.status !== 'cancel_requested'"
              class="button danger quiet"
              type="button"
              :disabled="acting"
              @click="cancel"
            >
              取消任务
            </button>
            <button
              v-if="job.status === 'failed'"
              class="button primary"
              type="button"
              :disabled="acting"
              @click="retry"
            >
              {{ acting ? "正在重试…" : "重试失败任务" }}
            </button>
            <RouterLink v-if="terminal" class="button quiet" to="/analyses">
              返回分析历史
            </RouterLink>
          </div>
        </div>
      </div>

      <div class="stage-list card">
        <div
          v-for="(stage, index) in stages"
          :key="stage.key"
          class="stage-row"
          :data-state="stageState(stage.threshold)"
        >
          <div class="stage-marker">
            <span>{{ stageState(stage.threshold) === "done" ? "✓" : index + 1 }}</span>
          </div>
          <div>
            <strong>{{ stage.label }}</strong>
          </div>
          <span class="stage-state">
            {{
              stageState(stage.threshold) === "done"
                ? "完成"
                : stageState(stage.threshold) === "active"
                  ? "进行中"
                  : "等待"
            }}
          </span>
        </div>
      </div>
    </template>
  </section>
</template>
