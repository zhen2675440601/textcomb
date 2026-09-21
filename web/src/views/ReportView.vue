<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { api, ApiProblem } from "@/api/client";
import type {
  FeedbackVerdict,
  Issue,
  IssueCategory,
  IssueLevel,
  Report,
  ReportSource,
} from "@/api/types";

const route = useRoute();
const router = useRouter();
const report = ref<Report | null>(null);
const loading = ref(true);
const error = ref("");
const category = ref<"all" | IssueCategory>("all");
const level = ref<"all" | IssueLevel>("all");
const feedbackFilter = ref<"all" | "unreviewed" | FeedbackVerdict>("all");
const minimumConfidence = ref(50);
const expanded = ref<Set<string>>(new Set());
const sendingFeedback = ref<string | null>(null);
const sourceOpen = ref(false);
const sourceLoading = ref(false);
const sourceError = ref("");
const source = ref<ReportSource | null>(null);
const selectedIssueId = ref<string | null>(null);
let sourceRequestVersion = 0;

const reportId = String(route.params.id);

const categoryLabels: Record<IssueCategory, string> = {
  typo: "错字",
  punctuation: "标点",
  grammar: "病句",
  paragraph: "分段",
};

const analysisProfileLabels: Record<"general" | "academic" | "financial", string> = {
  general: "通用文章",
  academic: "学术论文",
  financial: "财报/商业报告",
};

const subtypeLabels: Record<string, string> = {
  word_order: "语序不当",
  collocation: "搭配不当",
  missing_component: "成分残缺",
  redundant_component: "成分赘余",
  missing_or_redundant_component: "成分残缺或赘余",
  mixed_structure: "句式杂糅",
  ambiguity: "表意不明",
  illogical: "不合逻辑",
  conjunction: "关联词使用不当",
  word_misuse: "用词不当",
};

const visibleIssues = computed(() => {
  if (!report.value) return [];
  return report.value.issues.filter((issue) => {
    if (category.value !== "all" && issue.category !== category.value) return false;
    if (level.value !== "all" && issue.level !== level.value) return false;
    if (issue.confidence < minimumConfidence.value) return false;
    if (feedbackFilter.value === "unreviewed" && issue.feedback) return false;
    if (
      !["all", "unreviewed"].includes(feedbackFilter.value) &&
      issue.feedback !== feedbackFilter.value
    ) {
      return false;
    }
    return true;
  });
});

const sourceSelection = computed(() => {
  if (!source.value) return null;
  const issue = report.value?.issues.find((item) => item.id === selectedIssueId.value);
  if (!issue) return { before: source.value.text, selected: "", after: "" };
  if (
    issue.location.char_start < source.value.char_start ||
    issue.location.char_end > source.value.char_end
  ) {
    return { before: source.value.text, selected: "", after: "" };
  }

  const chars = Array.from(source.value.text);
  const start = Math.min(
    Math.max(issue.location.char_start - source.value.char_start, 0),
    chars.length,
  );
  const end = Math.min(
    Math.max(issue.location.char_end - source.value.char_start, start),
    chars.length,
  );
  return {
    before: chars.slice(0, start).join(""),
    selected: chars.slice(start, end).join(""),
    after: chars.slice(end).join(""),
  };
});

async function load() {
  loading.value = true;
  error.value = "";
  try {
    report.value = await api.report(reportId);
    const first = report.value.issues[0];
    if (first) {
      expanded.value.add(first.id);
      selectedIssueId.value = first.id;
    }
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "无法读取问题报告";
  } finally {
    loading.value = false;
  }
}

function toggle(issue: Issue) {
  selectedIssueId.value = issue.id;
  const next = new Set(expanded.value);
  if (next.has(issue.id)) next.delete(issue.id);
  else next.add(issue.id);
  expanded.value = next;
}

async function openSource(issue?: Issue) {
  if (issue) selectedIssueId.value = issue.id;
  sourceOpen.value = true;
  sourceError.value = "";

  const selectedIssue = issue ?? report.value?.issues.find((item) => item.id === selectedIssueId.value);
  const start = selectedIssue ? Math.max(0, selectedIssue.location.char_start - 3_000) : 0;
  const end = selectedIssue ? selectedIssue.location.char_end + 3_000 : 8_000;
  const coverageEnd = source.value ? Math.min(end, source.value.char_count) : end;
  if (
    source.value &&
    source.value.char_start <= start &&
    source.value.char_end >= coverageEnd
  ) {
    sourceRequestVersion += 1;
    sourceLoading.value = false;
    return;
  }

  const requestVersion = ++sourceRequestVersion;
  sourceLoading.value = true;
  try {
    const nextSource = await api.reportSource(reportId, start, end);
    if (requestVersion === sourceRequestVersion) source.value = nextSource;
  } catch (cause) {
    if (requestVersion === sourceRequestVersion) {
      sourceError.value = cause instanceof ApiProblem ? cause.message : "无法读取分析原文";
    }
  } finally {
    if (requestVersion === sourceRequestVersion) sourceLoading.value = false;
  }
}

function toggleSource() {
  if (sourceOpen.value) {
    sourceRequestVersion += 1;
    sourceLoading.value = false;
    sourceOpen.value = false;
    return;
  }
  void openSource(report.value?.issues.find((item) => item.id === selectedIssueId.value));
}

function scrollToSelectedSource() {
  document.getElementById("report-source-selection")?.scrollIntoView({
    behavior: "smooth",
    block: "center",
  });
}

watch([sourceOpen, source, selectedIssueId], () => {
  if (sourceOpen.value && source.value && sourceSelection.value?.selected) {
    void nextTick(scrollToSelectedSource);
  }
});

function locationLabel(issue: Issue) {
  const location = issue.location;
  if (location.page) {
    const lines =
      location.line_start && location.line_end
        ? `，第 ${location.line_start}–${location.line_end} 行`
        : "";
    return `第 ${location.page} 页${lines}`;
  }
  if (location.line_start) {
    return location.line_end && location.line_end !== location.line_start
      ? `第 ${location.line_start}–${location.line_end} 行`
      : `第 ${location.line_start} 行`;
  }
  if (location.paragraph_index !== undefined) {
    const sentence =
      location.sentence_index !== undefined
        ? `，第 ${location.sentence_index + 1} 句`
        : "";
    return `第 ${location.paragraph_index + 1} 段${sentence}`;
  }
  return `字符 ${location.char_start}–${location.char_end}`;
}

async function setFeedback(issue: Issue, verdict: FeedbackVerdict) {
  sendingFeedback.value = issue.id;
  error.value = "";
  try {
    await api.feedback(issue.id, verdict);
    issue.feedback = verdict;
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "反馈保存失败";
  } finally {
    sendingFeedback.value = null;
  }
}

async function removeReport() {
  if (!window.confirm("确定永久删除这份报告吗？分析原文、问题片段和 PDF 将不可恢复。")) return;
  try {
    await api.deleteReport(reportId);
    await router.replace("/analyses");
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "删除报告失败";
  }
}

function formatDate(value: string) {
  return new Intl.DateTimeFormat("zh-CN", {
    dateStyle: "long",
    timeStyle: "short",
  }).format(new Date(value));
}

onMounted(load);
</script>

<template>
  <section class="report-page page-stack">
    <div v-if="error" class="alert error">{{ error }}</div>
    <div v-if="loading" class="card loading-card">
      <span class="spinner" /> 正在打开问题报告…
    </div>

    <template v-else-if="report">
      <header class="report-header card">
        <div>
          <p class="eyebrow">TEXTCOMB REPORT · {{ report.schema }}</p>
          <h2>{{ report.document.original_name }}</h2>
          <p>
            {{ report.document.char_count.toLocaleString("zh-CN") }} 字符 ·
            {{ formatDate(report.generated_at) }}
          </p>
          <span class="quality-badge" :class="{ verified: report.analysis.reference_profile }">
            {{ report.analysis.reference_profile ? "质量验证配置" : "本次配置未经私有评测" }}
          </span>
        </div>
        <div class="report-actions">
          <div class="export-menu">
            <span>导出报告</span>
            <a :href="`/api/v1/reports/${report.report_id}/export/json`">JSON</a>
            <a :href="`/api/v1/reports/${report.report_id}/export/md`">Markdown</a>
            <a :href="`/api/v1/reports/${report.report_id}/export/pdf`">PDF</a>
          </div>
          <button class="button quiet compact" type="button" @click="toggleSource">
            {{ sourceOpen ? "收起原文" : "查看原文" }}
          </button>
          <button class="button danger quiet compact" type="button" @click="removeReport">
            删除
          </button>
        </div>
      </header>

      <div class="report-summary-grid">
        <article class="summary-card total">
          <span>发现问题</span>
          <strong>{{ report.summary.total }}</strong>
          <small>{{ report.summary.confirmed }} 项正式 · {{ report.summary.suspected }} 项疑似</small>
        </article>
        <article class="summary-card">
          <span>错字</span>
          <strong>{{ report.summary.typo }}</strong>
          <i class="category-dot typo" />
        </article>
        <article class="summary-card">
          <span>标点</span>
          <strong>{{ report.summary.punctuation }}</strong>
          <i class="category-dot punctuation" />
        </article>
        <article class="summary-card">
          <span>病句</span>
          <strong>{{ report.summary.grammar }}</strong>
          <i class="category-dot grammar" />
        </article>
        <article class="summary-card">
          <span>分段</span>
          <strong>{{ report.summary.paragraph }}</strong>
          <i class="category-dot paragraph" />
        </article>
      </div>

      <div class="report-workspace" :class="{ 'with-source': sourceOpen }">
        <aside class="filter-panel card">
          <div>
            <h3>筛选问题</h3>
            <button
              class="text-button"
              type="button"
              @click="
                category = 'all';
                level = 'all';
                feedbackFilter = 'all';
                minimumConfidence = 50;
              "
            >
              重置
            </button>
          </div>
          <label class="field compact-field">
            <span>问题类型</span>
            <select v-model="category">
              <option value="all">全部类型</option>
              <option value="typo">错字</option>
              <option value="punctuation">标点</option>
              <option value="grammar">病句</option>
              <option value="paragraph">分段</option>
            </select>
          </label>
          <label class="field compact-field">
            <span>问题级别</span>
            <select v-model="level">
              <option value="all">正式与疑似</option>
              <option value="confirmed">仅正式问题</option>
              <option value="suspected">仅疑似问题</option>
            </select>
          </label>
          <label class="field compact-field">
            <span>反馈状态</span>
            <select v-model="feedbackFilter">
              <option value="all">全部反馈</option>
              <option value="unreviewed">未反馈</option>
              <option value="correct">判断正确</option>
              <option value="incorrect">判断错误</option>
              <option value="disputed">有争议</option>
            </select>
          </label>
          <label class="range-field">
            <span>最低置信度 <strong>{{ minimumConfidence }}</strong></span>
            <input v-model.number="minimumConfidence" type="range" min="50" max="100" step="5" />
          </label>
          <p class="filter-result">当前显示 {{ visibleIssues.length }} / {{ report.issues.length }} 项</p>

          <div class="analysis-metadata">
            <h4>分析信息</h4>
            <dl>
              <div><dt>分析场景</dt><dd>{{ analysisProfileLabels[report.analysis.analysis_profile] ?? "通用文章" }}</dd></div>
              <div><dt>候选模型</dt><dd>{{ report.analysis.candidate_model }}</dd></div>
              <div><dt>复核模型</dt><dd>{{ report.analysis.verifier_model }}</dd></div>
              <div><dt>提示词版本</dt><dd>{{ report.analysis.prompt_version }}</dd></div>
              <div><dt>分析器版本</dt><dd>{{ report.analysis.analyzer_version }}</dd></div>
            </dl>
          </div>
        </aside>

        <div class="issue-column">
          <article
            v-for="(issue, index) in visibleIssues"
            :key="issue.id"
            class="issue-card"
            :data-category="issue.category"
            :class="{ expanded: expanded.has(issue.id) }"
          >
            <button class="issue-summary" type="button" @click="toggle(issue)">
              <span class="issue-number">{{ String(index + 1).padStart(2, "0") }}</span>
              <span class="issue-main">
                <span class="issue-labels">
                  <em>{{ categoryLabels[issue.category] }}</em>
                  <em v-if="issue.grammar_subtype" class="subtype">
                    {{ subtypeLabels[issue.grammar_subtype] ?? issue.grammar_subtype }}
                  </em>
                  <em class="level" :data-level="issue.level">
                    {{ issue.level === "confirmed" ? "正式问题" : "疑似问题" }}
                  </em>
                </span>
                <strong>“{{ issue.original_text }}”</strong>
                <small>{{ locationLabel(issue) }}</small>
              </span>
              <span class="confidence">
                <strong>{{ issue.confidence }}</strong>
                <small>置信度</small>
              </span>
              <span class="chevron">⌄</span>
            </button>

            <div v-if="expanded.has(issue.id)" class="issue-detail">
              <div class="detail-block">
                <span>问题原因</span>
                <p>{{ issue.reason }}</p>
              </div>
              <div class="detail-block suggestion">
                <span>修改建议</span>
                <p>{{ issue.suggestion }}</p>
              </div>
              <div v-if="issue.evidence_refs.length" class="evidence-list">
                <span>参考依据</span>
                <a
                  v-for="evidence in issue.evidence_refs"
                  :key="evidence.source_id"
                  :href="evidence.source_url"
                  target="_blank"
                  rel="noreferrer"
                >
                  {{ evidence.title }}（{{ evidence.revision }}）
                </a>
              </div>
              <div class="source-action-row">
                <button class="text-button" type="button" @click="openSource(issue)">
                  在分析原文中定位
                </button>
                <span>{{ locationLabel(issue) }}</span>
              </div>
              <div class="feedback-row">
                <span>这条判断是否有帮助？</span>
                <div>
                  <button
                    type="button"
                    :class="{ active: issue.feedback === 'correct' }"
                    :disabled="sendingFeedback === issue.id"
                    @click="setFeedback(issue, 'correct')"
                  >
                    ✓ 正确
                  </button>
                  <button
                    type="button"
                    :class="{ active: issue.feedback === 'incorrect' }"
                    :disabled="sendingFeedback === issue.id"
                    @click="setFeedback(issue, 'incorrect')"
                  >
                    × 错误
                  </button>
                  <button
                    type="button"
                    :class="{ active: issue.feedback === 'disputed' }"
                    :disabled="sendingFeedback === issue.id"
                    @click="setFeedback(issue, 'disputed')"
                  >
                    ? 有争议
                  </button>
                </div>
              </div>
            </div>
          </article>

          <div v-if="!visibleIssues.length" class="empty-state card compact-empty">
            <div class="empty-glyph">筛</div>
            <h3>没有符合条件的问题</h3>
            <p>调整左侧筛选条件，查看其他分析结果。</p>
          </div>
        </div>

        <aside v-if="sourceOpen" class="source-panel card">
          <header class="source-panel-header">
            <div>
              <p class="eyebrow">ANALYSIS SOURCE</p>
              <h3>分析原文</h3>
            </div>
            <button class="icon-button" type="button" aria-label="收起原文" @click="toggleSource">
              ×
            </button>
          </header>
          <p class="source-panel-note">显示用于定位问题的提取正文；DOCX 和 PDF 保留文字内容，不保留原始排版。</p>
          <div v-if="sourceLoading" class="inline-loading"><span class="spinner" /> 正在读取原文…</div>
          <div v-else-if="sourceError" class="source-unavailable">
            <strong>原文暂不可查看</strong>
            <p>{{ sourceError }}</p>
          </div>
          <div v-else-if="source" class="source-scroll">
            <p class="source-meta">
              {{ source.original_name }} ·
              {{ (source.char_start + 1).toLocaleString("zh-CN") }}–{{ source.char_end.toLocaleString("zh-CN") }} /
              {{ source.char_count.toLocaleString("zh-CN") }} 字符
            </p>
            <pre class="source-text"><template v-if="sourceSelection?.selected">{{ sourceSelection.before }}<mark id="report-source-selection">{{ sourceSelection.selected }}</mark>{{ sourceSelection.after }}</template><template v-else>{{ source.text }}</template></pre>
          </div>
        </aside>
      </div>

      <p class="report-disclaimer">
        文梳提供辅助分析，不代替作者、编辑或专业审校人员的最终判断。
      </p>
    </template>
  </section>
</template>
