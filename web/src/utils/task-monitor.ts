export type ConnectionState = "connecting" | "live" | "reconnecting" | "stopped";

interface MonitorOptions<T> {
  fetch: (signal: AbortSignal) => Promise<T>;
  subscribe: (receive: (value: T) => void, failed: () => void, opened: () => void) => { close(): void };
  terminal: (value: T) => boolean;
  update: (value: T) => void;
  connection: (state: ConnectionState) => void;
  // Return false for errors that need user action, such as an expired session.
  error: (cause: unknown) => boolean;
}

export function createTaskMonitor<T>(options: MonitorOptions<T>) {
  let stopped = false;
  let generation = 0;
  let attempts = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let requestTimeout: ReturnType<typeof setTimeout> | undefined;
  let controller: AbortController | undefined;
  let stream: { close(): void } | undefined;

  function clear() {
    generation += 1;
    clearTimeout(timer);
    clearTimeout(requestTimeout);
    timer = undefined;
    controller?.abort();
    controller = undefined;
    stream?.close();
    stream = undefined;
  }

  function stop() {
    stopped = true;
    clear();
    options.connection("stopped");
  }

  function receive(value: T) {
    options.update(value);
    if (options.terminal(value)) stop();
  }

  function reconnect() {
    if (stopped) return;
    clear();
    options.connection("reconnecting");
    timer = setTimeout(() => void refresh(), Math.min(1_500 * 2 ** attempts++, 15_000));
  }

  async function refresh() {
    if (stopped) return;
    clear();
    const current = generation;
    controller = new AbortController();
    requestTimeout = setTimeout(() => controller?.abort(), 20_000);
    try {
      const value = await options.fetch(controller.signal);
      if (stopped || current !== generation) return;
      clearTimeout(requestTimeout);
      receive(value);
      if (stopped) return;
      stream = options.subscribe(
        (value) => {
          if (stopped || current !== generation) return;
          attempts = 0;
          options.connection("live");
          receive(value);
        },
        () => { if (!stopped && current === generation) reconnect(); },
        () => { if (!stopped && current === generation) options.connection("live"); },
      );
    } catch (cause) {
      if (stopped || current !== generation) return;
      if (options.error(cause)) reconnect();
      else stop();
    }
  }

  options.connection("connecting");
  return { refresh, stop };
}
