<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { RouterLink } from "vue-router";
import { api, ApiProblem } from "@/api/client";
import type { Analysis, DocumentRecord } from "@/api/types";

const analyses = ref<Analysis[]>([]);
const documents = ref<DocumentRecord[]>([]);
const loading = ref(true);
const error = ref("");
const filter = ref<"all" | "active" | "completed" | "failed">("all");

const visibleAnalyses = computed(() => {
  if (filter.value === "all") return analyses.value;
  if (filter.value === "active") {
    return analyses.value.filter((item) =>
      ["queued", "extracting", "analyzing", "verifying", "merging", "rendering", "cancel_requested"].includes(
        item.status,
      ),
    );
  }
  return analyses.value.filter((item) => item.status === filter.value);
});

const documentNames = computed(
  () => new Map(documents.value.map((document) => [document.id, document.original_name])),
);

const completedCount = computed(
  () => analyses.value.filter((item) => item.status === "completed").length,
);
const activeCount = computed(
  () =>
    analyses.value.filter((item) =>
      ["queued", "extracting", "analyzing", "verifying", "merging", "rendering"].includes(
        item.status,
      ),
    ).length,
);

async function load() {
  loading.value = true;
  error.value = "";
  try {
    [analyses.value, documents.value] = await Promise.all([
      api.analyses(),
      api.documents(),
    ]);
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "加载分析历史失败";
  } finally {
    loading.value = false;
  }
}

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

onMounted(load);
</script>

<template>
  <section class="page-stack">
    <div class="stat-grid">
      <article class="stat-card featured">
        <span class="stat-label">全部分析</span>
        <strong>{{ analyses.length }}</strong>
        <p>每一篇文章都保留独立的问题报告。</p>
      </article>
      <article class="stat-card">
        <span class="stat-label">正在处理</span>
        <strong>{{ activeCount }}</strong>
        <p>任务在后台运行，可以随时离开页面。</p>
      </article>
      <article class="stat-card">
        <span class="stat-label">已完成</span>
        <strong>{{ completedCount }}</strong>
        <p>可导出 JSON、Markdown 与 PDF。</p>
      </article>
    </div>

    <div class="section-heading">
      <div>
        <h2>最近文章</h2>
        <p>跟踪分析状态，回到任意一份问题报告。</p>
      </div>
      <div class="segmented" aria-label="筛选任务">
        <button
          v-for="item in [
            ['all', '全部'],
            ['active', '处理中'],
            ['completed', '已完成'],
            ['failed', '失败'],
          ]"
          :key="item[0]"
          type="button"
          :class="{ active: filter === item[0] }"
          @click="filter = item[0] as typeof filter"
        >
          {{ item[1] }}
        </button>
      </div>
    </div>

    <div v-if="error" class="alert error">
      {{ error }}
      <button type="button" class="text-button" @click="load">重试</button>
    </div>

    <div v-if="loading" class="card loading-card">
      <span class="spinner" />
      正在整理你的文章…
    </div>

    <div v-else-if="visibleAnalyses.length" class="job-list">
      <article v-for="job in visibleAnalyses" :key="job.id" class="job-card">
        <div class="file-icon" :data-format="documents.find((doc) => doc.id === job.document_id)?.document_format">
          {{ documents.find((doc) => doc.id === job.document_id)?.document_format?.toUpperCase() ?? "DOC" }}
        </div>
        <div class="job-main">
          <div class="job-title-row">
            <div>
              <h3>{{ documentNames.get(job.document_id) ?? "已删除的文档" }}</h3>
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
      <h3>{{ filter === "all" ? "还没有分析记录" : "这个筛选下没有任务" }}</h3>
      <p>上传 TXT、DOCX 或文字型 PDF，开始第一次文章检查。</p>
      <RouterLink v-if="filter === 'all'" class="button primary" to="/analyses/new">
        新建分析
      </RouterLink>
    </div>
  </section>
</template>
