<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { api, ApiProblem, uploadDocument } from "@/api/client";
import type { AnalysisProfile, DocumentRecord, ModelProfile } from "@/api/types";

type SourceMode = "file" | "text";

const MAX_TEXT_CHARS = 200_000;
const router = useRouter();
const route = useRoute();
const exampleText = "今天我们讨论如何把文章写得更清楚。通过这次交流，使我们认识到认真校对的重要性。\n\n这段示例用于熟悉检查、原文定位和审核流程，不代表模型的实际准确率。";
const input = ref<HTMLInputElement>();
const selectedFile = ref<File | null>(null);
const sourceMode = ref<SourceMode>("file");
const pastedText = ref("");
const profiles = ref<ModelProfile[]>([]);
const selectedProfileId = ref("");
const analysisProfile = ref<AnalysisProfile>("general");
const loadingProfiles = ref(true);
const submitting = ref(false);
const uploadProgress = ref(0);
const error = ref("");
const dragging = ref(false);

const usableProfiles = computed(() => profiles.value.filter((profile) => profile.enabled));
const pastedCharCount = computed(() => Array.from(pastedText.value).length);
const pastedTextTooLong = computed(() => pastedCharCount.value > MAX_TEXT_CHARS);
const sourceReady = computed(() =>
  sourceMode.value === "file"
    ? Boolean(selectedFile.value)
    : !pastedTextTooLong.value && pastedText.value.trim().length > 0,
);
const selectedProfile = computed(() =>
  profiles.value.find((profile) => profile.id === selectedProfileId.value),
);

function selectFile(file?: File) {
  error.value = "";
  if (!file) return;
  const extension = file.name.split(".").pop()?.toLowerCase();
  if (!extension || !["txt", "docx", "pdf"].includes(extension)) {
    error.value = "请选择 TXT、DOCX 或文字型 PDF 文件";
    return;
  }
  if (file.size > 20 * 1024 * 1024) {
    error.value = "文件不能超过 20 MiB";
    return;
  }
  selectedFile.value = file;
  sourceMode.value = "file";
}

function onDrop(event: DragEvent) {
  dragging.value = false;
  selectFile(event.dataTransfer?.files[0]);
}

function chooseSource(mode: SourceMode) {
  error.value = "";
  sourceMode.value = mode;
}

function clearPastedText() {
  pastedText.value = "";
  error.value = "";
}

function formatSize(bytes: number) {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
}

async function startAnalysis() {
  if (!sourceReady.value || !selectedProfileId.value) return;
  const source = sourceMode.value;
  const file = selectedFile.value;
  const text = pastedText.value;
  error.value = "";
  submitting.value = true;
  uploadProgress.value = 0;
  let document: DocumentRecord | null = null;
  try {
    if (source === "file") {
      if (!file) return;
      document = await uploadDocument(file, (progress) => {
        uploadProgress.value = progress;
      });
    } else {
      uploadProgress.value = 35;
      document = await api.createTextDocument(text);
      uploadProgress.value = 100;
    }
    const job = await api.createAnalysis(
      document.id,
      selectedProfileId.value,
      analysisProfile.value,
    );
    await router.push(`/analyses/${job.id}`);
  } catch (cause) {
    error.value =
      cause instanceof ApiProblem
        ? cause.message
        : cause instanceof Error
          ? cause.message
          : "无法创建分析任务";
  } finally {
    submitting.value = false;
  }
}

async function loadProfiles() {
  try {
    profiles.value = await api.modelProfiles();
    const first = profiles.value.find((profile) => profile.enabled && profile.id === route.query.profile)
      ?? profiles.value.find((profile) => profile.enabled);
    selectedProfileId.value = first?.id ?? "";
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "无法读取模型配置";
  } finally {
    loadingProfiles.value = false;
  }
}

function fillExample() {
  if (pastedText.value.trim() && !window.confirm("用示例正文替换当前粘贴的内容吗？")) return;
  sourceMode.value = "text";
  pastedText.value = exampleText;
  error.value = "";
}

onMounted(() => { if (route.query.example === "1") fillExample(); void loadProfiles(); });
</script>

<template>
  <section class="new-analysis-page">
    <div class="page-stack">
      <div v-if="error" class="alert error" role="alert">{{ error }}</div>

      <section class="card form-card">
        <div class="step-heading">
          <span>01</span>
          <div>
            <h3>添加文章</h3>
            <button class="text-button" type="button" :disabled="submitting" @click="fillExample">填入示例正文</button>
          </div>
        </div>

        <div class="source-tabs" role="tablist" aria-label="文章来源">
          <button
            type="button"
            :class="{ selected: sourceMode === 'file' }"
            :aria-selected="sourceMode === 'file'"
            @click="chooseSource('file')"
          >
            上传文件
          </button>
          <button
            type="button"
            :class="{ selected: sourceMode === 'text' }"
            :aria-selected="sourceMode === 'text'"
            @click="chooseSource('text')"
          >
            粘贴正文
          </button>
        </div>

        <button
          v-if="sourceMode === 'file'"
          class="drop-zone"
          :class="{ dragging, selected: selectedFile }"
          type="button"
          @click="input?.click()"
          @dragenter.prevent="dragging = true"
          @dragover.prevent
          @dragleave.prevent="dragging = false"
          @drop.prevent="onDrop"
        >
          <input
            ref="input"
            class="sr-only"
            type="file"
            accept=".txt,.docx,.pdf,text/plain,application/vnd.openxmlformats-officedocument.wordprocessingml.document,application/pdf"
            @change="selectFile(($event.target as HTMLInputElement).files?.[0])"
          />
          <template v-if="selectedFile">
            <div class="upload-glyph selected">✓</div>
            <strong>{{ selectedFile.name }}</strong>
            <span>{{ formatSize(selectedFile.size) }} · 点击重新选择</span>
          </template>
          <template v-else>
            <div class="upload-glyph">↑</div>
            <strong>选择文件</strong>
            <span>TXT / DOCX / PDF · ≤20 MiB</span>
          </template>
        </button>

        <div v-else class="pasted-text-panel">
          <label class="sr-only" for="pasted-article">文章正文</label>
          <textarea
            id="pasted-article"
            v-model="pastedText"
            :aria-invalid="pastedTextTooLong"
            :disabled="submitting"
            placeholder="粘贴文章正文"
            rows="12"
          />
          <div class="pasted-text-meta">
            <span :class="{ over: pastedTextTooLong }">
              {{ pastedCharCount.toLocaleString() }} / {{ MAX_TEXT_CHARS.toLocaleString() }} 字
              <template v-if="pastedTextTooLong">（已超出上限）</template>
            </span>
            <button v-if="pastedText" class="text-button" type="button" @click="clearPastedText">
              清空正文
            </button>
          </div>
        </div>
      </section>

      <section class="card form-card">
        <div class="step-heading">
          <span>02</span>
          <div>
            <h3>选择文章场景</h3>
          </div>
        </div>
        <label class="field">
          <span>分析场景</span>
          <select v-model="analysisProfile">
            <option value="general">通用文章</option>
            <option value="academic">学术论文</option>
            <option value="financial">财报/商业报告</option>
          </select>
        </label>
      </section>

      <section class="card form-card">
        <div class="step-heading">
          <span>03</span>
          <div>
            <h3>选择分析模型</h3>
          </div>
        </div>

        <div v-if="loadingProfiles" class="inline-loading">
          <span class="spinner" /> 正在读取模型配置…
        </div>
        <div v-else-if="usableProfiles.length" class="profile-options">
          <label
            v-for="profile in usableProfiles"
            :key="profile.id"
            class="profile-option"
            :class="{ selected: selectedProfileId === profile.id }"
          >
            <input v-model="selectedProfileId" type="radio" :value="profile.id" />
            <span class="radio-dot" />
            <span>
              <strong>{{ profile.name }}</strong>
              <small>
                {{ profile.candidate_model }}
                <template v-if="profile.candidate_model !== profile.verifier_model">
                  ＋ {{ profile.verifier_model }}
                </template>
              </small>
            </span>
            <em :class="{ verified: profile.is_reference && analysisProfile === 'general' }">
              {{ profile.is_reference && analysisProfile === "general" ? "参考配置" : "未经验证" }}
            </em>
          </label>
        </div>
        <div v-else class="inline-empty">
          <p>还没有可用的模型配置。</p>
          <RouterLink class="button quiet compact" to="/models">前往配置</RouterLink>
        </div>
        <p v-if="selectedProfile" class="selected-provider">正文将发送至 {{ selectedProfile.base_url }}</p>
      </section>

      <div v-if="submitting" class="upload-progress-card">
        <div>
          <strong>
            {{ uploadProgress < 100 ? (sourceMode === "file" ? "正在上传" : "正在保存正文") : "正在创建分析任务" }}
          </strong>
          <span>{{ uploadProgress }}%</span>
        </div>
        <div class="mini-progress large">
          <span :style="{ width: `${uploadProgress}%` }" />
        </div>
      </div>

      <button
        class="button primary wide action-button"
        type="button"
        :disabled="!sourceReady || !selectedProfileId || submitting"
        @click="startAnalysis"
      >
        {{ submitting ? "正在提交…" : "开始分析" }}
        <span aria-hidden="true">→</span>
      </button>
    </div>
  </section>
</template>
