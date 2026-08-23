import "server-only";
import { API_SERVER_URL } from "@/lib/config";
import { requireAccessToken } from "@/lib/session";

// Reverse-proxy same-origin fin vers le pont HTTP+WS generique de
// `api-server` (`crates/api-server/src/vscode.rs::proxy_to_guest_port`,
// utilise par `vscode.rs` et `terminal.rs`) : le navigateur ne voit jamais
// le token, ajoute ici cote serveur a chaque requete. Couvre les assets
// HTTP normaux (HTML/JS/CSS) — le WebSocket "live" du service embarque est
// gere separement par `dashboard/server.ts` (les Route Handlers ne peuvent
// pas hijacker une connexion pour un upgrade).
//
// En-tetes de bout en bout retires avant de relayer dans les deux sens
// (`host`/`connection`) : propres a CETTE connexion (navigateur <->
// dashboard, puis dashboard <-> api-server), pas des donnees a transporter
// telles quelles.
const HOP_BY_HOP = new Set(["connection", "host"]);

// `fetch` (undici, cote Node) decompresse automatiquement le corps selon
// `Content-Encoding` (gzip/br/deflate), MAIS conserve cet en-tete tel quel
// dans `response.headers` (comportement standard fetch : seul le flux est
// decode, pas les en-tetes) — le relayer tel quel au navigateur lui ferait
// tenter de decompresser un corps deja en clair (`ERR_CONTENT_DECODING_FAILED`,
// constate en pratique sur `code-server`/`ttyd`, tous deux servis compresses).
// `content-length` doit suivre : il decrit la taille compressee d'origine,
// plus jamais correcte une fois le corps decompresse.
const DECODED_BY_FETCH = new Set(["content-encoding", "content-length"]);

/**
 * `service` est le segment d'URL commun cote `api-server` et cote dashboard
 * (`vscode` ou `terminal`) — seul ce qui differe entre les deux ponts.
 */
export async function guestProxy(
  req: Request,
  name: string,
  path: string[] | undefined,
  service: "vscode" | "terminal",
): Promise<Response> {
  const token = await requireAccessToken();

  const url = new URL(req.url);
  if ((!path || path.length === 0) && !url.pathname.endsWith("/")) {
    return Response.redirect(`${url.origin}${url.pathname}/${url.search}`, 307);
  }

  const targetPath = (path ?? []).join("/");
  const target = `${API_SERVER_URL}/v1/workshops/${encodeURIComponent(name)}/${service}/${targetPath}${url.search}`;

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

  if (response.status === 401) {
    return Response.redirect(`${url.origin}/login`, 307);
  }

  const responseHeaders = new Headers(response.headers);
  for (const key of HOP_BY_HOP) responseHeaders.delete(key);
  for (const key of DECODED_BY_FETCH) responseHeaders.delete(key);

  const location = responseHeaders.get("location");
  if (location) {
    const apiPrefix = `/v1/workshops/${encodeURIComponent(name)}/${service}`;
    const dashPrefix = `/workshops/${encodeURIComponent(name)}/${service}`;
    if (location.startsWith(apiPrefix)) {
      responseHeaders.set("location", dashPrefix + location.slice(apiPrefix.length));
    } else if (location.startsWith("/")) {
      responseHeaders.set("location", dashPrefix + location);
    }
  }

  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: responseHeaders,
  });
}
