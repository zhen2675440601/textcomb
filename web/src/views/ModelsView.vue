<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { api, ApiProblem } from "@/api/client";
import type { ModelProfile, ProviderKind } from "@/api/types";
import { useAuth } from "@/state/auth";

interface ProviderOption {
  kind: ProviderKind;
  label: string;
  defaultBaseUrl: string;
  endpointHint: string;
}

const providerOptions: ProviderOption[] = [
  {
    kind: "openai_responses",
    label: "OpenAI Responses",
    defaultBaseUrl: "https://api.openai.com/v1",
    endpointHint: "将在地址后使用 /responses，并以 Bearer 密钥认证。",
  },
  {
    kind: "openai_compatible",
    label: "OpenAI Compatible",
    defaultBaseUrl: "https://api.openai.com/v1",
    endpointHint: "将在地址后使用 /chat/completions，适用于兼容网关和本地服务。",
  },
  {
    kind: "anthropic",
    label: "Anthropic",
    defaultBaseUrl: "https://api.anthropic.com/v1",
    endpointHint: "将在地址后使用 /messages，并使用 Anthropic 原生认证头。",
  },
];

const defaultProvider = providerOptions[1]!;
const auth = useAuth();
const profiles = ref<ModelProfile[]>([]);
const loading = ref(true);
const saving = ref(false);
const testing = ref<string | null>(null);
const error = ref("");
const success = ref("");
const showForm = ref(false);
const editingProfileId = ref<string | null>(null);
const form = reactive({
  name: "",
  provider_kind: defaultProvider.kind,
  base_url: defaultProvider.defaultBaseUrl,
  api_key: "",
  candidate_model: "",
  verifier_model: "",
  max_concurrency: 10,
  shared: false,
  disclosure_accepted: false,
});

function providerFor(kind: ProviderKind): ProviderOption {
  return providerOptions.find((option) => option.kind === kind) ?? defaultProvider;
}

const activeProvider = computed(() => providerFor(form.provider_kind));
const isEditing = computed(() => editingProfileId.value !== null);

function resetForm() {
  Object.assign(form, {
    name: "",
    provider_kind: defaultProvider.kind,
    base_url: defaultProvider.defaultBaseUrl,
    api_key: "",
    candidate_model: "",
    verifier_model: "",
    max_concurrency: 10,
    shared: false,
    disclosure_accepted: false,
  });
}

function closeForm() {
  showForm.value = false;
  editingProfileId.value = null;
  resetForm();
}

function beginCreate() {
  error.value = "";
  success.value = "";
  editingProfileId.value = null;
  resetForm();
  showForm.value = true;
}

function beginEdit(profile: ModelProfile) {
  error.value = "";
  success.value = "";
  editingProfileId.value = profile.id;
  Object.assign(form, {
    name: profile.name,
    provider_kind: profile.provider_kind,
    base_url: profile.base_url,
    api_key: "",
    candidate_model: profile.candidate_model,
    verifier_model:
      profile.verifier_model === profile.candidate_model ? "" : profile.verifier_model,
    max_concurrency: profile.max_concurrency,
    shared: profile.shared,
    disclosure_accepted: false,
  });
  showForm.value = true;
}

function applyProviderDefaults() {
  form.base_url = activeProvider.value.defaultBaseUrl;
}

function providerLabel(kind: ProviderKind): string {
  return providerFor(kind).label;
}

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

async function save() {
  saving.value = true;
  error.value = "";
  success.value = "";
  try {
    const input = {
      name: form.name,
      provider_kind: form.provider_kind,
      base_url: form.base_url,
      api_key: form.api_key || undefined,
      candidate_model: form.candidate_model,
      verifier_model: form.verifier_model || undefined,
      max_concurrency: form.max_concurrency,
      disclosure_accepted: form.disclosure_accepted,
    };
    if (editingProfileId.value) {
      const profile = await api.updateModelProfile(editingProfileId.value, input);
      const index = profiles.value.findIndex((item) => item.id === profile.id);
      if (index >= 0) {
        profiles.value.splice(index, 1, profile);
      }
      success.value = "模型配置已更新。";
    } else {
      const profile = await api.createModelProfile({
        ...input,
        api_key: form.api_key,
        shared: form.shared,
      });
      profiles.value.unshift(profile);
      success.value = "模型配置已保存，密钥不会在后续页面中显示。";
    }
    closeForm();
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
        <p>支持 OpenAI Responses、OpenAI Compatible 和 Anthropic 三种接口格式。密钥经加密后保存，且永不回传。</p>
      </div>
      <button class="button primary" type="button" @click="showForm ? closeForm() : beginCreate()">
        {{ showForm ? (isEditing ? "取消修改" : "收起表单") : "添加模型配置" }}
      </button>
    </div>

    <div v-if="error" class="alert error">{{ error }}</div>
    <div v-if="success" class="alert success">{{ success }}</div>

    <form v-if="showForm" class="card provider-form" @submit.prevent="save">
      <div class="form-section-title">
        <span>{{ isEditing ? "修改配置" : "新配置" }}</span>
        <h3>{{ isEditing ? "修改" : "连接" }} {{ activeProvider.label }}</h3>
      </div>
      <div class="form-grid">
        <label class="field">
          <span>配置名称</span>
          <input v-model="form.name" required maxlength="100" placeholder="例如：我的主力模型" />
        </label>
        <label class="field">
          <span>接口格式</span>
          <select v-model="form.provider_kind" @change="applyProviderDefaults">
            <option v-for="option in providerOptions" :key="option.kind" :value="option.kind">
              {{ option.label }}
            </option>
          </select>
        </label>
        <label class="field full">
          <span>接口地址</span>
          <input
            v-model="form.base_url"
            required
            type="url"
            :placeholder="activeProvider.defaultBaseUrl"
          />
          <small class="field-hint">{{ activeProvider.endpointHint }}</small>
        </label>
        <label class="field full">
          <span>API 密钥</span>
          <input
            v-model="form.api_key"
            :required="!isEditing"
            type="password"
            autocomplete="new-password"
            :placeholder="isEditing ? '留空则保留当前密钥；输入新值可替换' : '仅本次输入，保存后不可查看'"
          />
        </label>
        <label class="field">
          <span>初检模型</span>
          <input v-model="form.candidate_model" required placeholder="模型标识" />
        </label>
        <label class="field">
          <span>复核模型（可选）</span>
          <input v-model="form.verifier_model" placeholder="留空则使用初检模型" />
        </label>
        <label class="field">
          <span>最大模型并发</span>
          <input v-model.number="form.max_concurrency" type="number" min="1" max="100" />
        </label>
      </div>
      <label v-if="auth.isAdmin.value && !isEditing" class="check-row">
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
        <button class="button quiet" type="button" @click="closeForm">取消</button>
        <button class="button primary" type="submit" :disabled="saving">
          {{ saving ? "正在加密保存…" : (isEditing ? "保存修改" : "保存配置") }}
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
            <span class="mini-badge">{{ providerLabel(profile.provider_kind) }}</span>
            <span v-if="profile.shared" class="mini-badge">共享</span>
            <span v-if="profile.is_reference" class="mini-badge verified">参考配置</span>
          </div>
          <p>{{ hostname(profile.base_url) }}</p>
          <dl>
            <div><dt>初检</dt><dd>{{ profile.candidate_model }}</dd></div>
            <div><dt>复核</dt><dd>{{ profile.verifier_model }}</dd></div>
            <div><dt>并发</dt><dd>{{ profile.max_concurrency }}</dd></div>
          </dl>
        </div>
        <div class="provider-actions">
          <span class="status-pill" :data-status="profile.enabled ? 'completed' : 'cancelled'">
            {{ profile.enabled ? "已启用" : "已停用" }}
          </span>
          <button class="button quiet compact" type="button" @click="beginEdit(profile)">
            修改
          </button>
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
      <p>添加任一种支持的模型服务，才能开始文章分析。</p>
      <button class="button primary" type="button" @click="beginCreate">添加配置</button>
    </div>

    <div class="security-strip">
      <span>密钥保护</span>
      <p>
        密钥使用 XChaCha20-Poly1305 加密，主密钥仅从运行环境读取。日志不会记录密钥、正文或完整提示词。
      </p>
    </div>
  </section>
</template>
