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
  import.meta.env.VITE_TEXTCOMB_SOURCE_URL ?? "https://github.com/textcomb/textcomb";

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
    <section class="login-story" aria-label="产品介绍">
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
      <div class="story-copy">
        <p class="eyebrow light">中文文章辅助校对</p>
        <h1>让文字的毛边，<br />在发表前被看见。</h1>
        <p>
          聚焦错字、标点、病句与分段建议。保留作者的判断权，
          也保留每一条建议的来由。
        </p>
      </div>
      <div class="story-features">
        <span>双轮 AI 复核</span>
        <span>原文精准定位</span>
        <span>三种报告导出</span>
      </div>
    </section>

    <section class="login-panel">
      <form class="login-form" @submit.prevent="submit">
        <div>
          <p class="eyebrow">WELCOME BACK</p>
          <h2>登录文梳</h2>
          <p class="muted">使用管理员为你创建的账号进入工作台。</p>
        </div>

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
        <p class="form-footnote">暂不开放公开注册。如需账号，请联系系统管理员。</p>
        <a class="source-link" :href="sourceUrl" target="_blank" rel="noreferrer">
          查看本服务对应的源代码（AGPL-3.0）
        </a>
      </form>
    </section>
  </main>
</template>
