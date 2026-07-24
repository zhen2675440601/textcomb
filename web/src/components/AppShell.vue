<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink, RouterView, useRoute, useRouter } from "vue-router";
import { useAuth } from "@/state/auth";

const auth = useAuth();
const route = useRoute();
const router = useRouter();
const mobileOpen = ref(false);
const sourceUrl =
  import.meta.env.VITE_TEXTCOMB_SOURCE_URL ?? "https://github.com/textcomb/textcomb";

const pageTitle = computed(() => {
  const titles: Record<string, string> = {
    history: "分析历史",
    "new-analysis": "新建分析",
    progress: "分析进度",
    report: "问题报告",
    models: "模型设置",
    "admin-users": "用户管理",
  };
  return titles[String(route.name)] ?? "文梳";
});

async function signOut() {
  await auth.logout();
  await router.replace({ name: "login" });
}
</script>

<template>
  <div class="app-shell">
    <aside class="sidebar" :class="{ open: mobileOpen }">
      <div class="brand">
        <div class="brand-mark" aria-hidden="true">
          <span />
          <span />
          <span />
        </div>
        <div>
          <strong>文梳</strong>
          <small>TextComb</small>
        </div>
      </div>

      <nav class="main-nav" aria-label="主导航" @click="mobileOpen = false">
        <RouterLink to="/analyses">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M5 4h14v16H5zM8 8h8M8 12h8M8 16h5" />
          </svg>
          分析历史
        </RouterLink>
        <RouterLink to="/analyses/new">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M12 5v14M5 12h14" />
          </svg>
          新建分析
        </RouterLink>
        <RouterLink to="/models">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="12" cy="12" r="3" />
            <path d="M19 12a7 7 0 0 0-.1-1l2-1.5-2-3.4-2.4 1A8 8 0 0 0 15 6l-.3-2.5h-4L10.3 6a8 8 0 0 0-1.6 1l-2.3-1-2 3.5 2 1.5a7 7 0 0 0 0 2l-2 1.5 2 3.5 2.3-1a8 8 0 0 0 1.6 1l.4 2.5h4L15 18a8 8 0 0 0 1.6-1l2.3 1 2-3.5-2-1.5a7 7 0 0 0 .1-1Z" />
          </svg>
          模型设置
        </RouterLink>
        <RouterLink v-if="auth.isAdmin.value" to="/admin/users">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="9" cy="8" r="3" />
            <path d="M3.5 19c.4-4 2-6 5.5-6s5.1 2 5.5 6M17 9v6M14 12h6" />
          </svg>
          用户管理
        </RouterLink>
      </nav>

      <div class="sidebar-note">
        <span class="eyebrow">辅助校对</span>
        <p>结论由 AI 提供，请由作者结合上下文最终确认。</p>
        <a :href="sourceUrl" target="_blank" rel="noreferrer">查看源代码（AGPL-3.0）</a>
      </div>

      <div class="account-card">
        <div class="avatar">{{ auth.user.value?.username.slice(0, 1).toUpperCase() }}</div>
        <div class="account-copy">
          <strong>{{ auth.user.value?.username }}</strong>
          <small>{{ auth.isAdmin.value ? "超级管理员" : "作者" }}</small>
        </div>
        <button class="icon-button" type="button" title="退出登录" @click="signOut">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M10 5H5v14h5M14 8l4 4-4 4M8 12h10" />
          </svg>
        </button>
      </div>
    </aside>

    <main class="main-area">
      <header class="topbar">
        <button
          class="icon-button mobile-menu"
          type="button"
          aria-label="打开菜单"
          @click="mobileOpen = !mobileOpen"
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M4 7h16M4 12h16M4 17h16" />
          </svg>
        </button>
        <div>
          <p class="eyebrow">TEXTCOMB WORKSPACE</p>
          <h1>{{ pageTitle }}</h1>
        </div>
        <RouterLink v-if="route.name !== 'new-analysis'" class="button primary compact" to="/analyses/new">
          <span aria-hidden="true">＋</span>
          新建分析
        </RouterLink>
      </header>
      <div class="page-content">
        <RouterView />
      </div>
    </main>
    <button
      v-if="mobileOpen"
      class="sidebar-scrim"
      aria-label="关闭菜单"
      @click="mobileOpen = false"
    />
  </div>
</template>
