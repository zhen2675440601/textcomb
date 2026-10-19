<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { RouterLink } from "vue-router";
import { api, ApiProblem } from "@/api/client";
import type { AnalysisHistoryPage, HistoryFilter } from "@/api/types";

const result = ref<AnalysisHistoryPage | null>(null);
const loading = ref(true);
const error = ref("");
const filter = ref<HistoryFilter>("all");
const search = ref("");
const appliedSearch = ref("");
const page = ref(1);
const totalPages = computed(() => Math.max(1, Math.ceil((result.value?.total ?? 0) / 20)));
const filters: { value: HistoryFilter; label: string }[] = [
  { value: "all", label: "全部" }, { value: "active", label: "处理中" },
  { value: "completed", label: "已完成" }, { value: "failed", label: "失败" },
  { value: "cancelled", label: "已取消" }, { value: "expired", label: "已过期" },
];
let controller: AbortController | undefined;
let refreshTimer: ReturnType<typeof setTimeout> | undefined;
let disposed = false;

async function load(quiet = false) {
  clearTimeout(refreshTimer);
  controller?.abort();
  const request = new AbortController();
  controller = request;
  if (!quiet) loading.value = true;
  error.value = "";
  try {
    const next = await api.analysisHistory(page.value, filter.value, appliedSearch.value, request.signal);
    if (disposed || request !== controller) return;
    result.value = next;
    page.value = next.page;
  } catch (cause) {
    if (disposed || request !== controller || request.signal.aborted) return;
    error.value = cause instanceof ApiProblem ? cause.message : "加载分析历史失败";
  } finally {
    if (!disposed && request === controller) {
      loading.value = false;
      if (result.value?.totals.active) refreshTimer = setTimeout(refresh, 8_000);
    }
  }
}

function refresh() {
  if (!document.hidden) void load(true);
  else refreshTimer = setTimeout(refresh, 8_000);
}

function visibilityChanged() {
  if (!document.hidden && result.value?.totals.active) void load(true);
}

function setFilter(value: HistoryFilter) { filter.value = value; page.value = 1; void load(); }
function findArticles() { appliedSearch.value = search.value.trim(); page.value = 1; void load(); }
function changePage(value: number) { page.value = value; void load(); }

function formatDate(value: string) {
  return new Intl.DateTimeFormat("zh-CN", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

const statusLabels: Record<string, string> = {
  queued: "等待中",
  extracting: "提取正文",
  analyzing: "候选分析",
  verifying: "复核问题",
  merging: "合并结果",
  rendering: "生成报告",
  completed: "已完成",
  failed: "失败",
  cancel_requested: "正在取消",
  cancelled: "已取消",
  expired: "已过期",
};

onMounted(() => { void load(); document.addEventListener("visibilitychange", visibilityChanged); });
onBeforeUnmount(() => {
  disposed = true;
  clearTimeout(refreshTimer);
  controller?.abort();
  document.removeEventListener("visibilitychange", visibilityChanged);
});
</script>

<template>
  <section class="page-stack">
    <div class="stat-grid">
      <article class="stat-card featured">
        <span class="stat-label">全部分析</span>
        <strong>{{ result?.totals.total ?? "—" }}</strong>
        <p>每一篇文章都保留独立的问题报告。</p>
      </article>
      <article class="stat-card">
        <span class="stat-label">正在处理</span>
        <strong>{{ result?.totals.active ?? "—" }}</strong>
        <p>任务在后台运行，可以随时离开页面。</p>
      </article>
      <article class="stat-card">
        <span class="stat-label">已完成</span>
        <strong>{{ result?.totals.completed ?? "—" }}</strong>
        <p>可导出 JSON、Markdown 与 PDF。</p>
      </article>
    </div>

    <div class="section-heading">
      <div>
        <h2>文章历史</h2>
        <p>跟踪分析状态，回到任意一份问题报告。</p>
      </div>
      <div class="segmented" aria-label="筛选任务">
        <button
          v-for="item in filters"
          :key="item.value"
          type="button"
          :class="{ active: filter === item.value }"
          :aria-pressed="filter === item.value"
          @click="setFilter(item.value)"
        >
          {{ item.label }}
        </button>
      </div>
    </div>

    <form class="history-search" role="search" @submit.prevent="findArticles">
      <label class="field"><span class="sr-only">搜索文章名</span><input v-model="search" type="search" maxlength="100" placeholder="搜索文章名" /></label>
      <button class="button quiet compact" type="submit">搜索</button>
      <button class="text-button" type="button" @click="load()">刷新列表</button>
      <span v-if="result?.totals.active" class="muted" role="status">处理中任务每 8 秒更新</span>
    </form>

    <div v-if="error" class="alert error" role="alert">
      {{ error }}
      <button type="button" class="text-button" @click="load()">重试</button>
    </div>

    <div v-if="loading" class="card loading-card">
      <span class="spinner" />
      正在整理你的文章…
    </div>

    <div v-else-if="result?.items.length" class="job-list">
      <article v-for="job in result.items" :key="job.id" class="job-card">
        <div class="file-icon" :data-format="job.document_format">
          {{ job.document_format.toUpperCase() }}
        </div>
        <div class="job-main">
          <div class="job-title-row">
            <div>
              <h3>{{ job.original_name }}</h3>
              <p>{{ formatDate(job.created_at) }} · {{ job.total_chunks || "—" }} 个文本块</p>
            </div>
            <span class="status-pill" :data-status="job.status">
              {{ statusLabels[job.status] ?? job.status }}
            </span>
          </div>
          <div v-if="!['completed', 'failed', 'cancelled', 'expired'].includes(job.status)" class="mini-progress">
            <span :style="{ width: `${job.progress}%` }" />
          </div>
          <p v-if="job.error_message" class="job-error">{{ job.error_message }}</p>
        </div>
        <RouterLink
          class="button quiet compact"
          :to="job.report_id ? `/reports/${job.report_id}` : `/analyses/${job.id}`"
        >
          {{ job.report_id ? "查看报告" : "查看进度" }}
          <span aria-hidden="true">→</span>
        </RouterLink>
      </article>
    </div>

    <div v-else class="empty-state card">
      <div class="empty-glyph">文</div>
      <h3>{{ result?.totals.total === 0 ? "开始你的第一次文章检查" : "没有匹配的文章" }}</h3>
      <p>上传 TXT、DOCX 或文字型 PDF，开始第一次文章检查。</p>
      <template v-if="result?.totals.total === 0">
        <p>先连接模型并测试，再用一段示例正文熟悉报告。</p>
        <RouterLink class="button quiet" to="/models">1. 连接模型</RouterLink>
        <RouterLink class="button primary" to="/analyses/new?example=1">2. 分析示例文章</RouterLink>
      </template>
    </div>

    <nav v-if="result && result.total > 0" class="history-pagination" aria-label="文章历史分页">
      <span>共 {{ result.total }} 篇 · 第 {{ page }} / {{ totalPages }} 页</span>
      <button class="button quiet compact" type="button" :disabled="page <= 1 || loading" @click="changePage(page - 1)">上一页</button>
      <button class="button quiet compact" type="button" :disabled="page >= totalPages || loading" @click="changePage(page + 1)">下一页</button>
    </nav>
  </section>
</template>
