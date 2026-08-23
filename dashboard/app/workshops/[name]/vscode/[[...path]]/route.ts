import "server-only";
import { API_SERVER_URL } from "@/lib/config";
import { requireAccessToken } from "@/lib/session";

// Reverse-proxy same-origin fin vers le pont HTTP+WS de `api-server`
// (`crates/api-server/src/vscode.rs`) : le navigateur ne voit jamais le
// token, ajoute ici cote serveur a chaque requete. Couvre les assets
// (HTML/JS/CSS) de `code-server` — le WebSocket "live" propre de
// `code-server` est gere separement par `dashboard/server.ts` (les Route
// Handlers ne peuvent pas hijacker une connexion pour un upgrade).
//
// En-tetes de bout en bout retires avant de relayer dans les deux sens
// (`host`/`connection`) : ce sont des en-tetes propres a CETTE connexion
// (navigateur <-> dashboard, puis dashboard <-> api-server), pas des
// donnees a transporter telles quelles.
const HOP_BY_HOP = new Set(["connection", "host"]);

async function proxy(req: Request, { params }: { params: Promise<{ name: string; path?: string[] }> }) {
  const token = await requireAccessToken();
  const { name, path } = await params;

  const url = new URL(req.url);
  const targetPath = (path ?? []).join("/");
  const target = `${API_SERVER_URL}/v1/workshops/${encodeURIComponent(name)}/vscode/${targetPath}${url.search}`;

  const headers = new Headers(req.headers);
  for (const key of HOP_BY_HOP) headers.delete(key);
  headers.set("Authorization", `Bearer ${token}`);

  const hasBody = req.method !== "GET" && req.method !== "HEAD";
  const response = await fetch(target, {
    method: req.method,
    headers,
    body: hasBody ? req.body : undefined,
    // Necessaire pour transmettre un corps en streaming (ReadableStream)
    // avec `fetch` cote Node.
    duplex: hasBody ? "half" : undefined,
    redirect: "manual",
    cache: "no-store",
  } as RequestInit & { duplex?: "half" });

  const responseHeaders = new Headers(response.headers);
  for (const key of HOP_BY_HOP) responseHeaders.delete(key);

  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: responseHeaders,
  });
}

export const GET = proxy;
export const POST = proxy;
export const PUT = proxy;
export const PATCH = proxy;
export const DELETE = proxy;
export const HEAD = proxy;
export const OPTIONS = proxy;
