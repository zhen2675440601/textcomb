import { createServer } from "node:http";

const port = Number(process.env.PORT ?? 4010);
const delayMs = Math.max(0, Number(process.env.FAKE_MODEL_DELAY_MS ?? 0));
const stats = { requests: 0, candidate: 0, verification: 0, connection_test: 0 };

function candidateIssues(text) {
  const fixtures = [
    {
      needle: "心情很繁重",
      issue: {
        category: "grammar",
        grammar_subtype: "collocation",
        quote: "心情很繁重",
        context_before: "",
        context_after: "",
        reason: "“繁重”通常不与“心情”搭配。",
        suggestion: "改为“心情很沉重”。",
        confidence: 94,
        evidence_source_ids: [],
      },
    },
    {
      needle: "通过这次学习，使",
      issue: {
        category: "grammar",
        grammar_subtype: "missing_component",
        quote: "通过这次学习，使",
        context_before: "",
        context_after: "",
        reason: "介词结构与“使”同时使用导致句子缺少主语。",
        suggestion: "删除“通过”或“使”。",
        confidence: 96,
        evidence_source_ids: [],
      },
    },
    {
      needle: "只要经常锻炼身体，才会",
      issue: {
        category: "grammar",
        grammar_subtype: "conjunction",
        quote: "只要经常锻炼身体，才会",
        context_before: "",
        context_after: "",
        reason: "“只要”应与“就”搭配。",
        suggestion: "将“才会”改为“就会”。",
        confidence: 97,
        evidence_source_ids: [],
      },
    },
  ];
  return fixtures
    .filter(({ needle }) => text.includes(needle))
    .map(({ issue }) => issue);
}

function messageContent(request) {
  const messages = Array.isArray(request.messages) ? request.messages : [];
  return {
    system: messages.find((message) => message.role === "system")?.content ?? "",
    user: messages.find((message) => message.role === "user")?.content ?? "",
  };
}

function responseFor(request) {
  const { system, user } = messageContent(request);
  if (user.includes('返回 {"ok": true}')) {
    stats.connection_test += 1;
    return { ok: true };
  }
  if (system.includes("复核员")) {
    stats.verification += 1;
    const marker = "候选 JSON：\n";
    const source = user.slice(user.indexOf(marker) + marker.length);
    let candidates = [];
    try {
      candidates = JSON.parse(source);
    } catch {
      candidates = [];
    }
    return {
      verdicts: candidates.map((candidate, candidate_index) => ({
        candidate_index,
        verdict: candidate.category === "paragraph" ? "suspected" : "confirmed",
        confidence: candidate.confidence ?? 90,
        reason: candidate.reason ?? "复核通过。",
        suggestion: candidate.suggestion ?? "",
        evidence_source_ids: candidate.evidence_source_ids ?? [],
      })),
    };
  }
  stats.candidate += 1;
  return { issues: candidateIssues(user) };
}

const server = createServer((request, response) => {
  if (request.method === "GET" && request.url === "/stats") {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify(stats));
    return;
  }
  if (request.method !== "POST" || request.url !== "/v1/chat/completions") {
    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: "not found" }));
    return;
  }
  let body = "";
  request.setEncoding("utf8");
  request.on("data", (chunk) => {
    body += chunk;
  });
  request.on("end", () => {
    let parsed;
    try {
      parsed = JSON.parse(body);
    } catch {
      response.writeHead(400, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: "invalid json" }));
      return;
    }
    stats.requests += 1;
    const send = () => {
      const content = JSON.stringify(responseFor(parsed));
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          id: "fake-textcomb-response",
          object: "chat.completion",
          choices: [{ index: 0, message: { role: "assistant", content } }],
          usage: { prompt_tokens: 100, completion_tokens: 40 },
        }),
      );
    };
    if (delayMs > 0) {
      setTimeout(send, delayMs);
    } else {
      send();
    }
  });
});

server.listen(port, "0.0.0.0", () => {
  process.stdout.write(`TextComb fake model listening on ${port}\n`);
});
