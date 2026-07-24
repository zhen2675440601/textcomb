<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { api, ApiProblem } from "@/api/client";
import type { ModelProfile } from "@/api/types";
import { useAuth } from "@/state/auth";

const auth = useAuth();
const profiles = ref<ModelProfile[]>([]);
const loading = ref(true);
const saving = ref(false);
const testing = ref<string | null>(null);
const error = ref("");
const success = ref("");
const showForm = ref(false);
const form = reactive({
  name: "",
  base_url: "https://api.openai.com/v1",
  api_key: "",
  candidate_model: "",
  verifier_model: "",
  max_concurrency: 10,
  shared: false,
  disclosure_accepted: false,
});

async function load() {
  loading.value = true;
  try {
    profiles.value = await api.modelProfiles();
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "无法读取模型配置";
  } finally {
    loading.value = false;
  }
}

async function create() {
  saving.value = true;
  error.value = "";
  success.value = "";
  try {
    const profile = await api.createModelProfile({
      ...form,
      verifier_model: form.verifier_model || undefined,
    });
    profiles.value.unshift(profile);
    Object.assign(form, {
      name: "",
      base_url: "https://api.openai.com/v1",
      api_key: "",
      candidate_model: "",
      verifier_model: "",
      max_concurrency: 10,
      shared: false,
      disclosure_accepted: false,
    });
    showForm.value = false;
    success.value = "模型配置已保存，密钥不会在后续页面中显示。";
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "保存模型配置失败";
  } finally {
    saving.value = false;
  }
}

async function test(profile: ModelProfile) {
  testing.value = profile.id;
  error.value = "";
  success.value = "";
  try {
    await api.testModelProfile(profile.id);
    success.value = `“${profile.name}”连接成功。`;
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "模型连接测试失败";
  } finally {
    testing.value = null;
  }
}

async function toggle(profile: ModelProfile) {
  error.value = "";
  try {
    await api.setModelProfileEnabled(profile.id, !profile.enabled);
    profile.enabled = !profile.enabled;
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "更新配置失败";
  }
}

function hostname(value: string) {
  try {
    return new URL(value).host;
  } catch {
    return value;
  }
}

onMounted(load);
</script>

<template>
  <section class="page-stack settings-page">
    <div class="section-heading">
      <div>
        <p class="eyebrow">MODEL PROVIDERS</p>
        <h2>模型服务</h2>
        <p>当前版本支持 OpenAI 兼容接口。密钥经加密后保存，且永不回传。</p>
      </div>
      <button class="button primary" type="button" @click="showForm = !showForm">
        {{ showForm ? "收起表单" : "添加模型配置" }}
      </button>
    </div>

    <div v-if="error" class="alert error">{{ error }}</div>
    <div v-if="success" class="alert success">{{ success }}</div>

    <form v-if="showForm" class="card provider-form" @submit.prevent="create">
      <div class="form-section-title">
        <span>新配置</span>
        <h3>连接 OpenAI 兼容服务</h3>
      </div>
      <div class="form-grid">
        <label class="field">
          <span>配置名称</span>
          <input v-model="form.name" required maxlength="100" placeholder="例如：我的主力模型" />
        </label>
        <label class="field">
          <span>接口地址</span>
          <input
            v-model="form.base_url"
            required
            type="url"
            placeholder="https://api.example.com/v1"
          />
        </label>
        <label class="field full">
          <span>API 密钥</span>
          <input
            v-model="form.api_key"
            required
            type="password"
            autocomplete="new-password"
            placeholder="仅本次输入，保存后不可查看"
          />
        </label>
        <label class="field">
          <span>候选分析模型</span>
          <input v-model="form.candidate_model" required placeholder="模型标识" />
        </label>
        <label class="field">
          <span>复核模型（可选）</span>
          <input v-model="form.verifier_model" placeholder="留空则使用候选模型" />
        </label>
        <label class="field">
          <span>最大模型并发</span>
          <input v-model.number="form.max_concurrency" type="number" min="1" max="100" />
        </label>
      </div>
      <label v-if="auth.isAdmin.value" class="check-row">
        <input v-model="form.shared" type="checkbox" />
        <span>设为所有用户可用的共享配置</span>
      </label>
      <label class="disclosure-check">
        <input v-model="form.disclosure_accepted" required type="checkbox" />
        <span>
          我已了解：文章正文会发送给此第三方模型服务商，并应自行确认该服务商的数据处理政策。
        </span>
      </label>
      <div class="form-actions">
        <button class="button quiet" type="button" @click="showForm = false">取消</button>
        <button class="button primary" type="submit" :disabled="saving">
          {{ saving ? "正在加密保存…" : "保存配置" }}
        </button>
      </div>
    </form>

    <div v-if="loading" class="card loading-card">
      <span class="spinner" /> 正在读取模型配置…
    </div>
    <div v-else-if="profiles.length" class="provider-list">
      <article v-for="profile in profiles" :key="profile.id" class="card provider-card">
        <div class="provider-logo">{{ profile.name.slice(0, 1).toUpperCase() }}</div>
        <div class="provider-main">
          <div>
            <h3>{{ profile.name }}</h3>
            <span v-if="profile.shared" class="mini-badge">共享</span>
            <span v-if="profile.is_reference" class="mini-badge verified">参考配置</span>
          </div>
          <p>{{ hostname(profile.base_url) }}</p>
          <dl>
            <div><dt>候选</dt><dd>{{ profile.candidate_model }}</dd></div>
            <div><dt>复核</dt><dd>{{ profile.verifier_model }}</dd></div>
            <div><dt>并发</dt><dd>{{ profile.max_concurrency }}</dd></div>
          </dl>
        </div>
        <div class="provider-actions">
          <span class="status-pill" :data-status="profile.enabled ? 'completed' : 'cancelled'">
            {{ profile.enabled ? "已启用" : "已停用" }}
          </span>
          <button
            class="button quiet compact"
            type="button"
            :disabled="testing === profile.id"
            @click="test(profile)"
          >
            {{ testing === profile.id ? "测试中…" : "测试连接" }}
          </button>
          <button class="text-button" type="button" @click="toggle(profile)">
            {{ profile.enabled ? "停用" : "启用" }}
          </button>
        </div>
      </article>
    </div>
    <div v-else class="empty-state card">
      <div class="empty-glyph">模</div>
      <h3>还没有模型配置</h3>
      <p>添加一个 OpenAI 兼容服务，才能开始文章分析。</p>
      <button class="button primary" type="button" @click="showForm = true">添加配置</button>
    </div>

    <div class="security-strip">
      <span>密钥保护</span>
      <p>
        密钥使用 XChaCha20-Poly1305 加密，主密钥仅从运行环境读取。日志不会记录密钥、正文或完整提示词。
      </p>
    </div>
  </section>
</template>
