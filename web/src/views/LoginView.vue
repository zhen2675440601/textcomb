<script setup lang="ts">
import { ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ApiProblem } from "@/api/client";
import { useAuth } from "@/state/auth";

const username = ref("");
const password = ref("");
const submitting = ref(false);
const error = ref("");
const auth = useAuth();
const route = useRoute();
const router = useRouter();
const sourceUrl =
  import.meta.env.VITE_TEXTCOMB_SOURCE_URL ?? "https://github.com/zhen2675440601/textcomb";

async function submit() {
  error.value = "";
  submitting.value = true;
  try {
    await auth.login(username.value.trim(), password.value);
    const redirect =
      typeof route.query.redirect === "string" ? route.query.redirect : "/analyses";
    await router.replace(redirect);
  } catch (cause) {
    error.value =
      cause instanceof ApiProblem ? cause.message : "暂时无法连接服务，请稍后重试";
  } finally {
    submitting.value = false;
  }
}
</script>

<template>
  <main class="login-page">
    <section class="login-panel">
      <form class="login-form" @submit.prevent="submit">
        <div class="login-brand">
          <div class="brand-mark large" aria-hidden="true">
            <span />
            <span />
            <span />
          </div>
          <div>
            <strong>文梳</strong>
            <small>TextComb</small>
          </div>
        </div>
        <h2>登录文梳</h2>

        <div v-if="error" class="alert error" role="alert">{{ error }}</div>

        <label class="field">
          <span>账号</span>
          <input
            v-model="username"
            autocomplete="username"
            autofocus
            required
            placeholder="输入账号"
          />
        </label>
        <label class="field">
          <span>密码</span>
          <input
            v-model="password"
            type="password"
            autocomplete="current-password"
            required
            placeholder="输入密码"
          />
        </label>
        <button class="button primary wide" type="submit" :disabled="submitting">
          {{ submitting ? "正在登录…" : "进入工作台" }}
        </button>
        <a class="source-link" :href="sourceUrl" target="_blank" rel="noreferrer">
          源代码（AGPL-3.0）
        </a>
      </form>
    </section>
  </main>
</template>
