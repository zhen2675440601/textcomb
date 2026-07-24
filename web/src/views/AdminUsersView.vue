<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { api, ApiProblem } from "@/api/client";
import type { AdminUser, SystemStatus } from "@/api/types";
import { useAuth } from "@/state/auth";

const auth = useAuth();
const users = ref<AdminUser[]>([]);
const systemStatus = ref<SystemStatus | null>(null);
const loading = ref(true);
const saving = ref(false);
const error = ref("");
const success = ref("");
const showCreate = ref(false);
const form = reactive({ username: "", password: "" });

async function load() {
  loading.value = true;
  try {
    const [nextUsers, nextSystemStatus] = await Promise.all([
      api.adminUsers(),
      api.systemStatus(),
    ]);
    users.value = nextUsers;
    systemStatus.value = nextSystemStatus;
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "无法读取用户";
  } finally {
    loading.value = false;
  }
}

async function create() {
  saving.value = true;
  error.value = "";
  try {
    await api.createUser(form.username.trim(), form.password);
    form.username = "";
    form.password = "";
    showCreate.value = false;
    success.value = "用户已创建，可以使用账号密码登录。";
    await load();
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "创建用户失败";
  } finally {
    saving.value = false;
  }
}

async function toggle(user: AdminUser) {
  const enabled = user.status !== "active";
  const verb = enabled ? "启用" : "停用";
  if (!window.confirm(`确定${verb}用户“${user.username}”吗？`)) return;
  error.value = "";
  try {
    await api.setUserEnabled(user.id, enabled);
    user.status = enabled ? "active" : "disabled";
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : `${verb}用户失败`;
  }
}

async function resetPassword(user: AdminUser) {
  const password = window.prompt(`为“${user.username}”输入新密码（至少 12 个字符）`);
  if (!password) return;
  error.value = "";
  try {
    await api.resetUserPassword(user.id, password);
    success.value = `“${user.username}”的密码已重置，旧会话已经失效。`;
  } catch (cause) {
    error.value = cause instanceof ApiProblem ? cause.message : "重置密码失败";
  }
}

function formatDate(value: string) {
  return new Intl.DateTimeFormat("zh-CN", { dateStyle: "medium" }).format(new Date(value));
}

onMounted(load);
</script>

<template>
  <section class="page-stack settings-page">
    <div class="section-heading">
      <div>
        <p class="eyebrow">ACCESS CONTROL</p>
        <h2>用户与访问</h2>
        <p>首版关闭公开注册。由超管为每位作者创建独立账号。</p>
      </div>
      <button class="button primary" type="button" @click="showCreate = !showCreate">
        {{ showCreate ? "取消" : "创建用户" }}
      </button>
    </div>

    <div v-if="error" class="alert error">{{ error }}</div>
    <div v-if="success" class="alert success">{{ success }}</div>

    <div v-if="systemStatus" class="stat-grid">
      <article class="stat-card featured">
        <span class="stat-label">系统状态</span>
        <strong>{{ systemStatus.database === "ready" ? "正常" : "异常" }}</strong>
        <p>{{ systemStatus.active_workers }} 个 Worker 正在持有任务租约。</p>
      </article>
      <article class="stat-card">
        <span class="stat-label">等待 / 处理中</span>
        <strong>{{ systemStatus.queued_jobs }} / {{ systemStatus.active_jobs }}</strong>
        <p>队列和当前处理中的分析任务。</p>
      </article>
      <article class="stat-card">
        <span class="stat-label">近 24 小时失败</span>
        <strong>{{ systemStatus.failed_jobs_last_24h }}</strong>
        <p>当前保留 {{ systemStatus.retained_reports }} 份未过期报告。</p>
      </article>
    </div>

    <form v-if="showCreate" class="card compact-form" @submit.prevent="create">
      <label class="field">
        <span>登录账号</span>
        <input v-model="form.username" required minlength="3" maxlength="64" autocomplete="off" />
      </label>
      <label class="field">
        <span>初始密码</span>
        <input
          v-model="form.password"
          required
          minlength="12"
          type="password"
          autocomplete="new-password"
        />
      </label>
      <button class="button primary" type="submit" :disabled="saving">
        {{ saving ? "正在创建…" : "创建普通用户" }}
      </button>
    </form>

    <div v-if="loading" class="card loading-card">
      <span class="spinner" /> 正在读取用户…
    </div>
    <div v-else class="card user-table-wrap">
      <table class="user-table">
        <thead>
          <tr>
            <th>用户</th>
            <th>角色</th>
            <th>状态</th>
            <th>创建日期</th>
            <th><span class="sr-only">操作</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="user in users" :key="user.id">
            <td>
              <div class="user-cell">
                <span class="avatar">{{ user.username.slice(0, 1).toUpperCase() }}</span>
                <strong>{{ user.username }}</strong>
                <em v-if="user.id === auth.user.value?.id">当前账号</em>
              </div>
            </td>
            <td>{{ user.role === "super_admin" ? "超级管理员" : "作者" }}</td>
            <td>
              <span class="status-pill" :data-status="user.status === 'active' ? 'completed' : 'cancelled'">
                {{ user.status === "active" ? "正常" : "已停用" }}
              </span>
            </td>
            <td>{{ formatDate(user.created_at) }}</td>
            <td>
              <div class="table-actions">
                <button class="text-button" type="button" @click="resetPassword(user)">
                  重置密码
                </button>
                <button
                  class="text-button danger-text"
                  type="button"
                  :disabled="user.id === auth.user.value?.id"
                  @click="toggle(user)"
                >
                  {{ user.status === "active" ? "停用" : "启用" }}
                </button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <div class="security-strip">
      <span>账号原则</span>
      <p>禁止多人共享超管账号。重置密码会立即删除该用户已有会话。</p>
    </div>
  </section>
</template>
