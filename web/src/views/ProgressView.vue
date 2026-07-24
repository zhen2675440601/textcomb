<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { api, ApiProblem } from "@/api/client";
import type { Analysis } from "@/api/types";

const route = useRoute();
const router = useRouter();
const job = ref<Analysis | null>(null);
const error = ref("");
const acting = ref(false);
let events: EventSource | null = null;

const jobId = String(route.params.id);
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
  if (job.value.progress >= threshold) return "done";
  const currentIndex = stages.findIndex((stage) => stage.key === job.value?.stage);
  const thisIndex = stages.findIndex((stage) => stage.threshold === threshold);
  return currentIndex === thisIndex ? "active" : "pending";
}

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

async function load() {
  try {
    job.value = await api.analysis(jobId);
    if (job.value.status === "completed" && job.value.report_id) {
      await router.replace(`/reports/${job.value.report_id}`);
      return;
    }
    if (!terminal.value) connect();
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "无法读取任务";
  }
}

function connect() {
  events?.close();
  events = new EventSource(`/api/v1/analyses/${jobId}/events`, {
    withCredentials: true,
  });
  events.addEventListener("progress", async (event) => {
    job.value = JSON.parse((event as MessageEvent).data) as Analysis;
    if (job.value.status === "completed" && job.value.report_id) {
      events?.close();
      await router.replace(`/reports/${job.value.report_id}`);
    } else if (terminal.value) {
      events?.close();
    }
  });
  events.onerror = () => {
    if (!terminal.value) {
      window.setTimeout(load, 3000);
    }
    events?.close();
  };
}

async function cancel() {
  if (!window.confirm("确定取消这次分析吗？已产生的模型调用无法撤回。")) return;
  acting.value = true;
  try {
    await api.cancelAnalysis(jobId);
    job.value = await api.analysis(jobId);
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
    job.value = await api.retryAnalysis(jobId);
    connect();
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "重试失败";
  } finally {
    acting.value = false;
  }
}

onMounted(load);
onBeforeUnmount(() => events?.close());
</script>

<template>
  <section class="progress-page">
    <div v-if="error" class="alert error">{{ error }}</div>
    <div v-if="!job" class="card loading-card">
      <span class="spinner" /> 正在读取任务…
    </div>

    <template v-else>
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
          <p class="eyebrow">{{ terminal ? "ANALYSIS STATUS" : "ANALYSIS IN PROGRESS" }}</p>
          <h2>{{ statusCopy }}</h2>
          <p v-if="job.status === 'failed'" class="failure-message">
            {{ job.error_message ?? "模型服务或文档处理发生错误。" }}
          </p>
          <p v-else-if="job.status === 'cancelled'">原文件与已提取正文已按策略清除。</p>
          <p v-else>
            已完成 {{ job.completed_chunks }} / {{ job.total_chunks || "—" }} 个文本块。
            你可以离开此页面，任务会继续在后台运行。
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
            <small v-if="stage.key === 'extracting'">读取格式并建立原文位置映射</small>
            <small v-else-if="stage.key === 'analyzing'">以高召回策略寻找错字、标点与病句</small>
            <small v-else-if="stage.key === 'verifying'">统一复核候选，降低正式问题误报</small>
            <small v-else-if="stage.key === 'merging'">核对原文引用并移除重叠项</small>
            <small v-else>输出 JSON、Markdown 与 PDF</small>
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
