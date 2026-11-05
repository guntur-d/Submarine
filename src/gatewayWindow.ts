// Browser-mode stand-in for the Tauri window handle.
///
// When the UI runs against `submarine serve` in a plain browser there is no
// OS window to control — no minimise/maximise/close, no resize persistence.
// `GatewayWindow` implements exactly the method subset the UI actually calls
// (see the `Pick` below — the compiler enforces the coverage), so
// DesktopApp/SessionView keep working untouched: effects early-return on
// no-ops, size queries read the browser viewport, and the title-bar buttons
// that would no-op are hidden separately (see `isGateway` in `./ipc`).
import {
  LogicalSize,
  PhysicalSize,
  getCurrentWindow,
  type CloseRequestedEvent,
  type Window,
} from "@tauri-apps/api/window";
import type { EventCallback, UnlistenFn } from "@tauri-apps/api/event";
import { IS_TAURI } from "./util/platform";

export type AppWindow = Pick<
  Window,
  | "isMaximized"
  | "onResized"
  | "toggleMaximize"
  | "setSize"
  | "innerSize"
  | "scaleFactor"
  | "onCloseRequested"
  | "minimize"
  | "close"
  | "destroy"
  | "outerSize"
>;

const noopUnlisten: UnlistenFn = () => {};

class GatewayWindow implements AppWindow {
  async isMaximized(): Promise<boolean> {
    return false;
  }
  async onResized(_handler: EventCallback<PhysicalSize>): Promise<UnlistenFn> {
    return noopUnlisten;
  }
  async toggleMaximize(): Promise<void> {
    // No OS window to toggle — the button is hidden in gateway mode.
  }
  async setSize(_size: LogicalSize | PhysicalSize): Promise<void> {
    // The browser owns the viewport; silently ignore like a denied request.
  }
  async innerSize(): Promise<PhysicalSize> {
    return new PhysicalSize(window.innerWidth, window.innerHeight);
  }
  async scaleFactor(): Promise<number> {
    return window.devicePixelRatio || 1;
  }
  async onCloseRequested(
    _handler: (event: CloseRequestedEvent) => void | Promise<void>,
  ): Promise<UnlistenFn> {
    return noopUnlisten;
  }
  async minimize(): Promise<void> {
    // Hidden in gateway mode — see above.
  }
  async close(): Promise<void> {
    // Hidden in gateway mode — never close the user's tab for them.
  }
  async destroy(): Promise<void> {
    // Hidden in gateway mode — see `close`.
  }
  async outerSize(): Promise<PhysicalSize> {
    return new PhysicalSize(window.outerWidth, window.outerHeight);
  }
}

let stub: GatewayWindow | null = null;

/// The window handle for the current runtime: the real Tauri window inside
/// the WebView, a no-op viewport-backed stub in gateway (browser) mode.
/// Stable across calls — safe to hold in `useState`.
export function getAppWindow(): AppWindow {
  if (IS_TAURI) return getCurrentWindow();
  if (!stub) stub = new GatewayWindow();
  return stub;
}
