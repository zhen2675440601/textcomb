<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useRouter } from "vue-router";
import { api, ApiProblem, uploadDocument } from "@/api/client";
import type { DocumentRecord, ModelProfile } from "@/api/types";

const router = useRouter();
const input = ref<HTMLInputElement>();
const selectedFile = ref<File | null>(null);
const profiles = ref<ModelProfile[]>([]);
const selectedProfileId = ref("");
const loadingProfiles = ref(true);
const submitting = ref(false);
const uploadProgress = ref(0);
const error = ref("");
const dragging = ref(false);

const usableProfiles = computed(() => profiles.value.filter((profile) => profile.enabled));
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
}

function onDrop(event: DragEvent) {
  dragging.value = false;
  selectFile(event.dataTransfer?.files[0]);
}

function formatSize(bytes: number) {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
}

async function startAnalysis() {
  if (!selectedFile.value || !selectedProfileId.value) return;
  error.value = "";
  submitting.value = true;
  uploadProgress.value = 0;
  let document: DocumentRecord | null = null;
  try {
    document = await uploadDocument(selectedFile.value, (progress) => {
      uploadProgress.value = progress;
    });
    const job = await api.createAnalysis(document.id, selectedProfileId.value);
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
    const first = profiles.value.find((profile) => profile.enabled);
    selectedProfileId.value = first?.id ?? "";
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "无法读取模型配置";
  } finally {
    loadingProfiles.value = false;
  }
}

onMounted(loadProfiles);
</script>

<template>
  <section class="new-analysis-grid">
    <div class="page-stack">
      <div class="intro-copy">
        <p class="eyebrow">NEW ANALYSIS</p>
        <h2>把文章交给文梳检查</h2>
        <p>原文件仅用于本次分析。成功、取消或主动删除后会立即清除。</p>
      </div>

      <div v-if="error" class="alert error" role="alert">{{ error }}</div>

      <section class="card form-card">
        <div class="step-heading">
          <span>01</span>
          <div>
            <h3>选择文章</h3>
            <p>支持 UTF-8 / GB18030 TXT、DOCX 与文字型 PDF。</p>
          </div>
        </div>

        <button
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
            <strong>拖放文章到这里，或点击选择</strong>
            <span>单文件不超过 20 MiB，正文不超过 5 万字</span>
          </template>
        </button>
      </section>

      <section class="card form-card">
        <div class="step-heading">
          <span>02</span>
          <div>
            <h3>选择分析模型</h3>
            <p>模型会读取文章正文；请确认服务商的数据政策。</p>
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
            <em :class="{ verified: profile.is_reference }">
              {{ profile.is_reference ? "参考配置" : "未经验证" }}
            </em>
          </label>
        </div>
        <div v-else class="inline-empty">
          <p>还没有可用的模型配置。</p>
          <RouterLink class="button quiet compact" to="/models">前往配置</RouterLink>
        </div>
      </section>

      <div v-if="submitting" class="upload-progress-card">
        <div>
          <strong>{{ uploadProgress < 100 ? "正在上传" : "正在创建分析任务" }}</strong>
          <span>{{ uploadProgress }}%</span>
        </div>
        <div class="mini-progress large">
          <span :style="{ width: `${uploadProgress}%` }" />
        </div>
      </div>

      <button
        class="button primary wide action-button"
        type="button"
        :disabled="!selectedFile || !selectedProfileId || submitting"
        @click="startAnalysis"
      >
        {{ submitting ? "正在提交…" : "开始分析" }}
        <span aria-hidden="true">→</span>
      </button>
    </div>

    <aside class="expectation-card">
      <p class="eyebrow">本次会检查</p>
      <ul>
        <li><span>字</span><div><strong>错字</strong><small>疑似错别字与不规范字</small></div></li>
        <li><span>，</span><div><strong>标点</strong><small>用法、位置与全半角问题</small></div></li>
        <li><span>句</span><div><strong>八类病句</strong><small>语序、搭配、成分、结构等</small></div></li>
        <li><span>¶</span><div><strong>分段建议</strong><small>主题变化未分段，仅作疑似提醒</small></div></li>
      </ul>
      <div class="privacy-note">
        <strong>数据处理提醒</strong>
        <p>正文会发送给所选模型服务商。文梳不会在日志中记录正文、问题片段或密钥。</p>
      </div>
      <p v-if="selectedProfile" class="selected-provider">
        当前服务：{{ selectedProfile.base_url }}
      </p>
    </aside>
  </section>
</template>
