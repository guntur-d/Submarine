// IPC boundary between the UI and the backend.
//
// Desktop (`tauri dev` / built app): every call passes straight through to
// Tauri (`invoke`/`listen`) — zero behaviour change.
// Gateway (`submarine serve` + `?gateway=` in the URL): the same calls ride
// a WebSocket using the serve protocol (`{id,cmd,args}` ↔ `{id,ok,result}`,
// `{type:sub/unsub}` + `{event,payload}` pushes). Call sites never know.
//
// Signatures mirror `@tauri-apps/api/{core,event}` exactly — the swap is
// transparent. Binary `invoke` args (ArrayBuffer/Uint8Array) are rejected
// upfront in gateway mode: JSON can't carry them, and silent corruption
// would be worse than a loud error (binary transfer arrives in 2c).
import {
  invoke as tauriInvoke,
  type InvokeArgs,
  type InvokeOptions,
} from "@tauri-apps/api/core";
import {
  listen as tauriListen,
  type Event,
  type EventCallback,
  type EventName,
  type EventTarget,
  type Options as EventOptions,
  type UnlistenFn,
} from "@tauri-apps/api/event";
import { IS_TAURI } from "./util/platform";

export type {
  EventCallback,
  EventName,
  EventTarget,
  EventOptions,
  InvokeArgs,
  InvokeOptions,
  UnlistenFn,
};

export function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  options?: InvokeOptions,
): Promise<T> {
  if (!isGateway()) return tauriInvoke<T>(cmd, args, options);
  if (args !== undefined && hasBinary(args)) {
    return Promise.reject(
      new Error("[ipc] binary invoke args are not supported in gateway mode yet (2c)"),
    );
  }
  return gatewayInvoke<T>(cmd, args ?? {});
}

export function listen<T>(
  event: EventName,
  handler: EventCallback<T>,
  options?: EventOptions,
): Promise<UnlistenFn> {
  if (!isGateway()) return tauriListen<T>(event, handler, options);
  // `options.target` has no meaning against a single gateway — one server,
  // one broadcast bus. Accepted for signature parity, otherwise ignored.
  return gatewayListen<T>(String(event), handler);
}

// ---------------------------------------------------------------------------
// Gateway mode: config
// ---------------------------------------------------------------------------

export interface GatewayConfig {
  url: string;
  token: string;
}

const GATEWAY_URL_KEY = "submarine-gateway-url";
const GATEWAY_TOKEN_KEY = "submarine-gateway-token";

// WS endpoint for a gateway served from the page's own origin
// (`submarine serve` prints plain `?token=` links, so the WS lives on the
// same host/port — http(s) maps to ws(s) 1:1).
function sameOriginWsUrl(): string {
  const wsProto = window.location.protocol === "https:" ? "wss" : "ws";
  return `${wsProto}://${window.location.host}/api/ws`;
}

function readGatewayFromLocation(): GatewayConfig | null {
  try {
    const q = new URLSearchParams(window.location.search);
    const token = q.get("token");
    if (!token) return null;
    // Explicit `?gateway=` wins (remote gateway, reverse proxy, custom port).
    const url = q.get("gateway") ?? sameOriginWsUrl();
    try {
      sessionStorage.setItem(GATEWAY_URL_KEY, url);
      sessionStorage.setItem(GATEWAY_TOKEN_KEY, token);
      // Scrub the secret from the address bar (history/bookmarks/shoulder
      // surfing). Reloads keep working: the stored copy is the source of
      // truth from here on; the URL form is only the first-load bootstrap.
      history.replaceState(null, "", window.location.pathname + window.location.hash);
    } catch {
      // Private mode etc. — the URL params still work for this page load.
    }
    return { url, token };
  } catch {
    return null;
  }
}

function readGatewayFromStorage(): GatewayConfig | null {
  try {
    const url = sessionStorage.getItem(GATEWAY_URL_KEY);
    const token = sessionStorage.getItem(GATEWAY_TOKEN_KEY);
    return url && token ? { url, token } : null;
  } catch {
    return null;
  }
}

let gatewayCache: GatewayConfig | null | undefined;
export function getGateway(): GatewayConfig | null {
  if (gatewayCache === undefined) {
    gatewayCache = readGatewayFromLocation() ?? readGatewayFromStorage();
  }
  return gatewayCache;
}

export function isGateway(): boolean {
  return !IS_TAURI && getGateway() !== null;
}

// Manual token entry for the hint card (open base URL in another browser,
// paste the token from the server console). Derives the WS endpoint
// same-origin, exactly like the `?token=` bootstrap, and warms the cache
// so the very next render mounts the app — no reload needed.
export function rememberGatewayToken(token: string): boolean {
  token = token.trim();
  if (!token) return false;
  const url = sameOriginWsUrl();
  try {
    sessionStorage.setItem(GATEWAY_URL_KEY, url);
    sessionStorage.setItem(GATEWAY_TOKEN_KEY, token);
  } catch {
    return false;
  }
  gatewayCache = { url, token };
  return true;
}

// Drop the stored gateway config (stale/rejected token, user reset).
// Next render falls back to the static hint card; a fresh `?token=` URL
// re-bootstraps from scratch.
export function forgetGateway(): void {
  gatewayCache = null;
  try {
    sessionStorage.removeItem(GATEWAY_URL_KEY);
    sessionStorage.removeItem(GATEWAY_TOKEN_KEY);
  } catch {
    // ignore — storage unavailable, memory cache is already cleared
  }
  try {
    window.dispatchEvent(new CustomEvent("submarine-gateway-invalid"));
  } catch {
    // ignore — no listeners yet
  }
}

// Probe whether the stored token is still accepted. Returns "ok" (server
// up, token good), "unauthorized" (server up, token dead — storage is
// cleared as a side effect), or "unreachable" (don't touch storage; the
// server may just be starting).
export async function checkGateway(): Promise<"ok" | "unauthorized" | "unreachable"> {
  const base = gatewayHttpBase();
  const token = getGateway()?.token;
  if (!base || !token) return "unreachable";
  let res: Response;
  try {
    res = await fetch(`${base}/api/ping?token=${encodeURIComponent(token)}`);
  } catch {
    return "unreachable";
  }
  if (res.ok) return "ok";
  if (res.status === 401) {
    forgetGateway();
    return "unauthorized";
  }
  return "unreachable";
}

// HTTP base for the gateway's REST endpoints (`/api/files/*`). Derived from
// the WS URL the same way a browser would: ws(s)://host/api/ws -> http(s)://host.
export function gatewayHttpBase(): string | null {
  const gw = getGateway();
  if (!gw) return null;
  const http = gw.url.replace(/^ws:\/\//, "http://").replace(/^wss:\/\//, "https://");
  const idx = http.indexOf("/api/ws");
  return idx >= 0 ? http.slice(0, idx) : http;
}

// ---------------------------------------------------------------------------
// Gateway mode: WebSocket transport
// ---------------------------------------------------------------------------

type Pending = { resolve: (v: any) => void; reject: (e: any) => void };

const pending = new Map<number, Pending>();
const eventHandlers = new Map<string, Set<EventCallback<any>>>();
const ackWaiters = new Map<string, Array<() => void>>();
let wsNextId = 1;
let ws: WebSocket | null = null;
let wsReady: Promise<WebSocket> | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

function hasBinary(v: unknown): boolean {
  if (v instanceof ArrayBuffer || v instanceof Uint8Array) return true;
  if (Array.isArray(v)) return v.some(hasBinary);
  if (v !== null && typeof v === "object") return Object.values(v).some(hasBinary);
  return false;
}

function failAllPending(err: Error) {
  for (const [id, p] of pending) {
    pending.delete(id);
    p.reject(err);
  }
}

function resolveAcks(events: string[]) {
  for (const e of events) {
    const waiters = ackWaiters.get(e);
    if (waiters) {
      ackWaiters.delete(e);
      waiters.forEach((r) => r());
    }
  }
}

function resolveAllAcks() {
  for (const [, waiters] of ackWaiters) waiters.forEach((r) => r());
  ackWaiters.clear();
}

function scheduleReconnect() {
  if (reconnectTimer !== null) return;
  // Stay down when nobody cares — the next invoke/listen reconnects lazily.
  // Otherwise a dead gateway turns into a forever console-spam loop.
  if (pending.size === 0 && eventHandlers.size === 0) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    ensureSocket().catch(() => scheduleReconnect());
  }, 1500);
}

function routeMessage(data: unknown) {
  if (typeof data !== "string") return;
  let msg: any;
  try {
    msg = JSON.parse(data);
  } catch {
    return;
  }
  if (msg && typeof msg.event === "string") {
    const handlers = eventHandlers.get(msg.event);
    if (handlers) {
      const full = { event: msg.event, id: 0, payload: msg.payload } as Event<any>;
      handlers.forEach((h) => {
        try {
          h(full);
        } catch (e) {
          console.error("[ipc] event handler threw:", e);
        }
      });
    }
    return;
  }
  if (msg && msg.type === "sub-ack" && Array.isArray(msg.events)) {
    resolveAcks(msg.events);
    return;
  }
  if (msg && typeof msg.id === "number") {
    const p = pending.get(msg.id);
    if (!p) return;
    pending.delete(msg.id);
    if (msg.ok) p.resolve(msg.result);
    else p.reject(new Error(typeof msg.error === "string" ? msg.error : "gateway error"));
  }
}

function ensureSocket(): Promise<WebSocket> {
  if (ws && ws.readyState === WebSocket.OPEN) return Promise.resolve(ws);
  if (wsReady) return wsReady;
  wsReady = new Promise<WebSocket>((resolve, reject) => {
    const gw = getGateway();
    if (!gw) {
      wsReady = null;
      reject(new Error("[ipc] no gateway configured"));
      return;
    }
    const sep = gw.url.includes("?") ? "&" : "?";
    let w: WebSocket;
    try {
      w = new WebSocket(`${gw.url}${sep}token=${encodeURIComponent(gw.token)}`);
    } catch (e) {
      wsReady = null;
      reject(e);
      return;
    }
    w.onopen = () => {
      ws = w;
      // Server-side subscriptions are per-connection — re-register everything
      // live so a reconnect is invisible to call sites.
      const events = [...eventHandlers.keys()];
      if (events.length > 0) w.send(JSON.stringify({ type: "sub", events }));
      resolve(w);
    };
    w.onmessage = (ev) => routeMessage(ev.data);
    w.onclose = () => {
      if (ws === w) ws = null;
      wsReady = null;
      // Handlers stay registered (they'll be re-subbed on reconnect), but a
      // sub-ack they were waiting on will never arrive — resolve them so
      // `listen()` doesn't hang; the registration is re-established anyway.
      resolveAllAcks();
      failAllPending(new Error("[ipc] gateway disconnected"));
      reject(new Error("[ipc] gateway connection failed"));
      // A dead token looks identical to a dead server at the WS layer
      // (both just close). Probe over HTTP: 401 means the token is stale
      // (server restarted?) — forget it so the UI shows the re-connect
      // hint instead of retrying a doomed token forever.
      checkGateway().then((st) => {
        if (st !== "unauthorized") scheduleReconnect();
      });
    };
  });
  return wsReady;
}

function gatewayInvoke<T>(cmd: string, args: InvokeArgs): Promise<T> {
  return ensureSocket().then(
    (w) =>
      new Promise<T>((resolve, reject) => {
        const id = wsNextId++;
        pending.set(id, { resolve: resolve as (v: any) => void, reject });
        try {
          w.send(JSON.stringify({ id, cmd, args }));
        } catch (e) {
          // Socket died between connect and send — don't leak the entry;
          // the close handler already scheduled a reconnect for live users.
          pending.delete(id);
          reject(e);
        }
      }),
  );
}

function gatewayListen<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
  // Registered synchronously so nothing can slip through before the sub
  // request goes out (single-threaded: no event can be routed in between).
  let set = eventHandlers.get(event);
  if (!set) {
    set = new Set();
    eventHandlers.set(event, set);
  }
  set.add(handler as EventCallback<any>);
  const removeHandler = () => {
    const s = eventHandlers.get(event);
    if (!s) return;
    s.delete(handler as EventCallback<any>);
    if (s.size === 0) {
      eventHandlers.delete(event);
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "unsub", events: [event] }));
      }
    }
  };
  return ensureSocket()
    .then((w) => {
      w.send(JSON.stringify({ type: "sub", events: [event] }));
      return new Promise<UnlistenFn>((resolve) => {
        const waiters = ackWaiters.get(event) ?? [];
        waiters.push(() => resolve(removeHandler));
        ackWaiters.set(event, waiters);
      });
    })
    .catch((e) => {
      removeHandler();
      throw e;
    });
}
