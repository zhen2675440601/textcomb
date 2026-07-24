import { createRouter, createWebHistory } from "vue-router";
import { useAuth } from "@/state/auth";
import AppShell from "@/components/AppShell.vue";
import LoginView from "@/views/LoginView.vue";
import HistoryView from "@/views/HistoryView.vue";
import NewAnalysisView from "@/views/NewAnalysisView.vue";
import ProgressView from "@/views/ProgressView.vue";
import ReportView from "@/views/ReportView.vue";
import ModelsView from "@/views/ModelsView.vue";
import AdminUsersView from "@/views/AdminUsersView.vue";

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    {
      path: "/login",
      name: "login",
      component: LoginView,
      meta: { public: true },
    },
    {
      path: "/",
      component: AppShell,
      children: [
        { path: "", redirect: "/analyses" },
        { path: "analyses", name: "history", component: HistoryView },
        { path: "analyses/new", name: "new-analysis", component: NewAnalysisView },
        { path: "analyses/:id", name: "progress", component: ProgressView },
        { path: "reports/:id", name: "report", component: ReportView },
        { path: "models", name: "models", component: ModelsView },
        {
          path: "admin/users",
          name: "admin-users",
          component: AdminUsersView,
          meta: { admin: true },
        },
      ],
    },
    { path: "/:pathMatch(.*)*", redirect: "/" },
  ],
});

router.beforeEach(async (to) => {
  const auth = useAuth();
  await auth.initialize();
  if (!to.meta.public && !auth.user.value) {
    return { name: "login", query: { redirect: to.fullPath } };
  }
  if (to.name === "login" && auth.user.value) {
    return { name: "history" };
  }
  if (to.meta.admin && !auth.isAdmin.value) {
    return { name: "history" };
  }
  return true;
});
