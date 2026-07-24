import { computed, reactive } from "vue";
import { api } from "@/api/client";
import type { User } from "@/api/types";

const state = reactive<{
  user: User | null;
  initialized: boolean;
}>({
  user: null,
  initialized: false,
});

let initialization: Promise<void> | null = null;

async function initialize(): Promise<void> {
  if (state.initialized) return;
  if (!initialization) {
    initialization = api
      .me()
      .then((user) => {
        state.user = user;
      })
      .catch(() => {
        state.user = null;
      })
      .finally(() => {
        state.initialized = true;
      });
  }
  await initialization;
}

async function login(username: string, password: string): Promise<void> {
  state.user = await api.login(username, password);
  state.initialized = true;
}

async function logout(): Promise<void> {
  try {
    await api.logout();
  } finally {
    state.user = null;
    state.initialized = true;
  }
}

export function useAuth() {
  return {
    state,
    user: computed(() => state.user),
    isAdmin: computed(() => state.user?.role === "super_admin"),
    initialize,
    login,
    logout,
  };
}
