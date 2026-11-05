import { useEffect, useReducer, useState } from "react";
import DesktopApp from "./DesktopApp";
import { ConfirmProvider } from "./ui/confirm";
import { BroadcastProvider } from "./ui/broadcast";
import { IS_TAURI } from "./util/platform";
import { checkGateway, getGateway, rememberGatewayToken } from "./ipc";

// Token entry for the no-gateway hint card. Covers the "opened the base URL
// in another browser" flow: the address bar never holds the token (it is
// scrubbed on load), so pasting it here is the way in.
function GatewayTokenForm({ onSaved }: { onSaved: () => void }) {
  const [value, setValue] = useState("");
  return (
    <form
      className="text-sm space-y-2"
      onSubmit={(e) => {
        e.preventDefault();
        if (rememberGatewayToken(value)) onSaved();
      }}
    >
      <p className="text-zinc-500">Or paste a gateway token from the server console:</p>
      <div className="flex gap-2">
        <input
          type="password"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="token…"
          autoComplete="off"
          className="flex-1 min-w-0 bg-black/40 rounded px-3 py-2 text-[13px] text-emerald-300 outline-none border border-white/10 focus:border-primary/50 font-mono"
        />
        <button
          type="submit"
          className="shrink-0 px-3 py-2 rounded bg-primary/20 text-primary text-[13px] font-bold hover:bg-primary/30 transition-colors"
        >
          Connect
        </button>
      </div>
    </form>
  );
}

function App() {
  // Tracks a stored gateway token that the server has rejected (restarted
  // server = new token). Flips via the `submarine-gateway-invalid` event
  // (fired by forgetGateway) or the mount-time probe below.
  const [gatewayInvalid, setGatewayInvalid] = useState(false);
  // Bumped when a token is pasted into the hint card, forcing a re-render
  // so the boundary below re-reads getGateway() and mounts the app.
  const [, bump] = useReducer((x: number) => x + 1, 0);
  useEffect(() => {
    if (IS_TAURI) return;
    const onInvalid = () => setGatewayInvalid(true);
    window.addEventListener("submarine-gateway-invalid", onInvalid);
    // Mount-time probe: a tab restored from sessionStorage may hold a token
    // from a previous server boot. Failing fast here beats mounting the app
    // against a dead token (endless WS retries + cryptic console errors).
    if (getGateway()) {
      checkGateway().then((st) => {
        if (st === "unauthorized") setGatewayInvalid(true);
      });
    }
    return () => window.removeEventListener("submarine-gateway-invalid", onInvalid);
  }, []);

  // Single runtime boundary. A plain browser with no Tauri runtime and no
  // gateway configured gets the static hint (`npm run dev` with no backend
  // can neither call `getCurrentWindow()` nor `invoke()` anything). With
  // `?gateway=` (+ stored token), the full app mounts against `serve`.
  // A rejected token gets its own message: the fix is a fresh URL from the
  // server console, not the Tauri commands below.
  if (!IS_TAURI && (!getGateway() || gatewayInvalid)) {
    const invalid = gatewayInvalid;
    return (
      <div className="min-h-screen flex items-center justify-center bg-[#0a0a0c] text-zinc-300 p-6">
        <div className="max-w-md w-full bg-[#121216] border border-white/10 rounded-xl p-6 space-y-3">
          <h1 className="text-white font-bold text-lg">
            {invalid ? "Gateway token rejected" : "Submarine runs in Tauri"}
          </h1>
          {invalid ? (
            <>
              <p className="text-sm text-zinc-400">
                The saved gateway token was rejected — the server was probably
                restarted (each boot mints a new token). Re-open the URL printed
                by <code className="text-emerald-300">submarine serve</code> in
                its console, or paste the fresh token below.
              </p>
              <GatewayTokenForm onSaved={() => { setGatewayInvalid(false); bump(); }} />
            </>
          ) : (
            <>
              <p className="text-sm text-zinc-400">
                You opened the Vite dev server in a plain browser. The UI needs the Tauri
                runtime (window APIs + Rust backend), so it can't run here.
              </p>
              <div className="text-sm space-y-1">
                <p className="text-zinc-500">Run one of these instead:</p>
                <code className="block bg-black/40 rounded px-3 py-2 text-[13px] text-emerald-300">npm run tauri dev</code>
                <code className="block bg-black/40 rounded px-3 py-2 text-[13px] text-emerald-300">npm run android:dev</code>
              </div>
              <GatewayTokenForm onSaved={() => bump()} />
            </>
          )}
        </div>
      </div>
    );
  }

  return (
    <ConfirmProvider>
      <BroadcastProvider>
        <DesktopApp />
      </BroadcastProvider>
    </ConfirmProvider>
  );
}

export default App;
