import http from "k6/http";
import { check, sleep } from "k6";

export const options = {
  scenarios: {
    ten_long_articles: {
      executor: "per-vu-iterations",
      vus: 10,
      iterations: 1,
      maxDuration: "125m",
    },
  },
  thresholds: {
    http_req_failed: ["rate<0.01"],
    checks: ["rate>0.99"],
  },
};

const baseUrl = __ENV.TEXTCOMB_BASE_URL || "http://localhost:3000";
const username = __ENV.TEXTCOMB_USERNAME;
const password = __ENV.TEXTCOMB_PASSWORD;
const modelProfileId = __ENV.TEXTCOMB_MODEL_PROFILE_ID;
const paragraph = "这是用于验证长文任务并发、队列租约和报告隔离的可控测试段落。";
const documentText = `${paragraph}\n\n`.repeat(Math.ceil(50000 / paragraph.length)).slice(0, 50000);

export default function () {
  const login = http.post(
    `${baseUrl}/api/v1/auth/login`,
    JSON.stringify({ username, password }),
    { headers: { "content-type": "application/json" } },
  );
  check(login, { "login succeeds": (response) => response.status === 200 });
  const cookie = login.cookies.textcomb_session?.[0]?.value;
  const headers = { Cookie: `textcomb_session=${cookie}` };

  const upload = http.post(
    `${baseUrl}/api/v1/documents`,
    { file: http.file(documentText, `load-${__VU}.txt`, "text/plain") },
    { headers },
  );
  check(upload, { "upload succeeds": (response) => response.status === 201 });
  const documentId = upload.json("id");

  const create = http.post(
    `${baseUrl}/api/v1/analyses`,
    JSON.stringify({
      document_id: documentId,
      model_profile_id: modelProfileId,
    }),
    {
      headers: {
        ...headers,
        "content-type": "application/json",
        "idempotency-key": `k6-${__VU}-${Date.now()}`,
      },
    },
  );
  check(create, { "analysis accepted": (response) => [200, 202].includes(response.status) });
  const jobId = create.json("id");

  let terminal = false;
  for (let attempt = 0; attempt < 720 && !terminal; attempt += 1) {
    sleep(10);
    const status = http.get(`${baseUrl}/api/v1/analyses/${jobId}`, { headers });
    check(status, { "status readable": (response) => response.status === 200 });
    terminal = ["completed", "failed", "cancelled", "expired"].includes(status.json("status"));
    if (terminal) {
      check(status, { "analysis completed": (response) => response.json("status") === "completed" });
    }
  }
}
