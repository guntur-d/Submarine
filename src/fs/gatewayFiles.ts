import { gatewayHttpBase, getGateway } from "../ipc";

// Browser-side file bridge for gateway mode. The WS transport is JSON-only
// (binary invoke args are rejected), so file bytes ride plain HTTP:
//   upload   POST {base}/api/files/upload?token=  (multipart: session_id,
//            remote_path, overwrite, file)
//   download GET  {base}/api/files/download?session_id=&remote_path=&token=
// Both endpoints already exist server-side (serve.rs); this module is the
// missing frontend half. Auth reuses the gateway token from sessionStorage
// (same secret the WS uses); nothing new to configure.

function baseAndToken(): { base: string; token: string } {
  const base = gatewayHttpBase();
  const token = getGateway()?.token;
  if (!base || !token) throw new Error("gateway not configured");
  return { base, token };
}

export async function uploadBrowserFiles(
  sessionId: string,
  remoteDir: string,
  files: FileList | File[],
  overwrite = false,
): Promise<number> {
  const { base, token } = baseAndToken();
  const dir = remoteDir.replace(/[\\/]+$/, "");
  if (!dir) throw new Error("open a remote directory first");
  const list = Array.from(files);
  let done = 0;
  for (const f of list) {
    const form = new FormData();
    form.append("session_id", sessionId);
    form.append("remote_path", `${dir}/${f.name}`);
    if (overwrite) form.append("overwrite", "true");
    form.append("file", f, f.name);
    const res = await fetch(`${base}/api/files/upload?token=${encodeURIComponent(token)}`, {
      method: "POST",
      body: form,
    });
    if (!res.ok) throw new Error(`upload HTTP ${res.status}`);
    const body = (await res.json()) as { ok: boolean; error?: string };
    if (!body.ok) throw new Error(body.error || "upload failed");
    done++;
  }
  return done;
}

export async function downloadRemoteFile(
  sessionId: string,
  remotePath: string,
): Promise<void> {
  const { base, token } = baseAndToken();
  const name = remotePath.split("/").pop() || "download";
  const url =
    `${base}/api/files/download?session_id=${encodeURIComponent(sessionId)}` +
    `&remote_path=${encodeURIComponent(remotePath)}&token=${encodeURIComponent(token)}`;
  const res = await fetch(url);
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(text || `download HTTP ${res.status}`);
  }
  const blob = await res.blob();
  const obj = URL.createObjectURL(blob);
  try {
    const a = document.createElement("a");
    a.href = obj;
    a.download = name;
    document.body.appendChild(a);
    a.click();
    a.remove();
  } finally {
    setTimeout(() => URL.revokeObjectURL(obj), 5000);
  }
}
