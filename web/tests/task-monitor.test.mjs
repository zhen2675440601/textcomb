import { test } from "node:test";
import assert from "node:assert/strict";
import { createTaskMonitor } from "../src/utils/task-monitor.ts";

function fixture(t, fetch) {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const subscriptions = [];
  const updates = [];
  const states = [];
  const monitor = createTaskMonitor({
    fetch,
    subscribe(receive, failed, opened) {
      const stream = { receive, failed, opened, closed: false, close() { this.closed = true; } };
      subscriptions.push(stream);
      return stream;
    },
    terminal: (job) => job.status === "completed",
    update: (job) => updates.push(job),
    connection: (state) => states.push(state),
    error: (error) => error.status !== 401,
  });
  return { monitor, subscriptions, updates, states };
}
const flush = async () => { for (let n = 0; n < 6; n++) await Promise.resolve(); };

test("failed event stream recovers even if the first fallback HTTP request also fails", async (t) => {
  let calls = 0;
  const f = fixture(t, async () => { calls++; if (calls === 2) throw Error("offline"); return { status: calls >= 3 ? "completed" : "analyzing" }; });
  await f.monitor.refresh();
  f.subscriptions[0].failed();
  assert.equal(f.subscriptions[0].closed, true);
  t.mock.timers.tick(1500); await flush();
  assert.equal(calls, 2);
  t.mock.timers.tick(3000); await flush();
  assert.equal(f.updates.at(-1).status, "completed");
  assert.equal(f.states.at(-1), "stopped");
});

test("navigation cancels reconnection timers and ignores late event callbacks", async (t) => {
  let calls = 0;
  const f = fixture(t, async () => { calls++; return { status: "analyzing" }; });
  await f.monitor.refresh();
  f.subscriptions[0].failed();
  f.monitor.stop();
  t.mock.timers.tick(60000); await flush();
  f.subscriptions[0].receive({ status: "completed" });
  assert.equal(calls, 1);
  assert.equal(f.updates.length, 1);
});

test("stopping aborts an in-flight fetch and ignores its delayed response", async (t) => {
  let finish;
  let signal;
  const f = fixture(t, (requestSignal) => { signal = requestSignal; return new Promise((resolve) => { finish = resolve; }); });
  const pending = f.monitor.refresh();
  f.monitor.stop();
  assert.equal(signal.aborted, true);
  finish({ status: "analyzing" }); await pending;
  assert.equal(f.subscriptions.length, 0);
  assert.equal(f.updates.length, 0);
});

test("expired authentication stops retries instead of reconnecting forever", async (t) => {
  let calls = 0;
  const f = fixture(t, async () => { calls++; throw { status: 401 }; });
  await f.monitor.refresh();
  t.mock.timers.tick(60000); await flush();
  assert.equal(calls, 1);
  assert.equal(f.states.at(-1), "stopped");
});

test("a stalled HTTP request times out and schedules recovery", async (t) => {
  let calls = 0;
  const f = fixture(t, (signal) => {
    calls++;
    if (calls > 1) return Promise.resolve({ status: "completed" });
    return new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(Error("timeout")), { once: true }));
  });
  const pending = f.monitor.refresh();
  t.mock.timers.tick(20000); await pending;
  assert.equal(f.states.at(-1), "reconnecting");
  t.mock.timers.tick(1500); await flush();
  assert.equal(f.updates.at(-1).status, "completed");
});
