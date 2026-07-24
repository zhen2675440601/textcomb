import type {
  AdminUser,
  SystemStatus,
  Analysis,
  DocumentRecord,
  FeedbackVerdict,
  ModelProfile,
  ProblemDetails,
  Report,
  User,
} from "./types";

const API_ROOT = "/api/v1";

export class ApiProblem extends Error {
  readonly status: number;
  readonly code: string;
  readonly requestId?: string;

  constructor(problem: ProblemDetails) {
    super(problem.detail || problem.title);
    this.name = "ApiProblem";
    this.status = problem.status;
    this.code = problem.code;
    this.requestId = problem.request_id;
  }
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  if (init.body && !(init.body instanceof FormData) && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  const response = await fetch(`${API_ROOT}${path}`, {
    ...init,
    headers,
    credentials: "include",
  });
  if (!response.ok) {
    let problem: ProblemDetails;
    try {
      problem = (await response.json()) as ProblemDetails;
    } catch {
      problem = {
        type: "about:blank",
        title: "请求失败",
        status: response.status,
        code: "HTTP_ERROR",
        detail: `服务器返回 ${response.status}`,
      };
    }
    throw new ApiProblem(problem);
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

function idempotencyKey(): string {
  return crypto.randomUUID();
}

export const api = {
  login: (username: string, password: string) =>
    request<User>("/auth/login", {
      method: "POST",
      body: JSON.stringify({ username, password }),
    }),
  logout: () => request<void>("/auth/logout", { method: "POST" }),
  me: () => request<User>("/me"),

  documents: () => request<DocumentRecord[]>("/documents"),
  deleteDocument: (id: string) =>
    request<void>(`/documents/${id}`, { method: "DELETE" }),

  analyses: () => request<Analysis[]>("/analyses"),
  analysis: (id: string) => request<Analysis>(`/analyses/${id}`),
  deleteAnalysis: (id: string) =>
    request<void>(`/analyses/${id}`, { method: "DELETE" }),
  createAnalysis: (documentId: string, modelProfileId: string) =>
    request<Analysis>("/analyses", {
      method: "POST",
      headers: { "Idempotency-Key": idempotencyKey() },
      body: JSON.stringify({
        document_id: documentId,
        model_profile_id: modelProfileId,
      }),
    }),
  cancelAnalysis: (id: string) =>
    request<void>(`/analyses/${id}/cancel`, { method: "POST" }),
  retryAnalysis: (id: string) =>
    request<Analysis>(`/analyses/${id}/retry`, {
      method: "POST",
      headers: { "Idempotency-Key": idempotencyKey() },
    }),

  report: (id: string) => request<Report>(`/reports/${id}`),
  deleteReport: (id: string) =>
    request<void>(`/reports/${id}`, { method: "DELETE" }),
  feedback: (issueId: string, verdict: FeedbackVerdict, note?: string) =>
    request<void>(`/issues/${issueId}/feedback`, {
      method: "POST",
      body: JSON.stringify({ verdict, note }),
    }),

  modelProfiles: () => request<ModelProfile[]>("/model-profiles"),
  createModelProfile: (input: {
    name: string;
    base_url: string;
    api_key: string;
    candidate_model: string;
    verifier_model?: string;
    max_concurrency: number;
    shared: boolean;
    disclosure_accepted: boolean;
  }) =>
    request<ModelProfile>("/model-profiles", {
      method: "POST",
      body: JSON.stringify(input),
    }),
  testModelProfile: (id: string) =>
    request<{ ok: boolean }>(`/model-profiles/${id}/test`, { method: "POST" }),
  setModelProfileEnabled: (id: string, enabled: boolean) =>
    request<void>(`/model-profiles/${id}/enabled`, {
      method: "PATCH",
      body: JSON.stringify({ enabled }),
    }),

  systemStatus: () => request<SystemStatus>("/admin/system-status"),
  adminUsers: () => request<AdminUser[]>("/admin/users"),
  createUser: (username: string, password: string) =>
    request<User>("/admin/users", {
      method: "POST",
      body: JSON.stringify({ username, password }),
    }),
  setUserEnabled: (id: string, enabled: boolean) =>
    request<void>(`/admin/users/${id}/status`, {
      method: "PATCH",
      body: JSON.stringify({ enabled }),
    }),
  resetUserPassword: (id: string, password: string) =>
    request<void>(`/admin/users/${id}/password`, {
      method: "POST",
      body: JSON.stringify({ password }),
    }),
};

export function uploadDocument(
  file: File,
  onProgress: (percent: number) => void,
): Promise<DocumentRecord> {
  return new Promise((resolve, reject) => {
    const form = new FormData();
    form.append("file", file);
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `${API_ROOT}/documents`);
    xhr.withCredentials = true;
    xhr.upload.addEventListener("progress", (event) => {
      if (event.lengthComputable) {
        onProgress(Math.round((event.loaded / event.total) * 100));
      }
    });
    xhr.addEventListener("load", () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve(JSON.parse(xhr.responseText) as DocumentRecord);
        return;
      }
      try {
        reject(new ApiProblem(JSON.parse(xhr.responseText) as ProblemDetails));
      } catch {
        reject(new Error(`上传失败（${xhr.status}）`));
      }
    });
    xhr.addEventListener("error", () => reject(new Error("网络连接失败")));
    xhr.send(form);
  });
}
