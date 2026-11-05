import { useEffect } from "react";
import { listen, type EventCallback } from "../ipc";

// Subscribe to a backend event via the IPC boundary (see `src/ipc.ts`).
// Handles the unmount-before-listen-resolves race via a `cancelled` flag.
// Pass `enabled = false` to skip subscribing.
export function useIpcListen<T>(
  event: string | null | undefined,
  handler: EventCallback<T>,
  deps: React.DependencyList,
  enabled: boolean = true,
) {
  useEffect(() => {
    if (!event || !enabled) return;
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    listen<T>(event, handler).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}
