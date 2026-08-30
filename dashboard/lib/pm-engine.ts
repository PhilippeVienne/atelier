import "server-only";
import { PM_ENGINE_URL } from "./config";
import { requireAccessToken } from "./session";

// Client BFF pour `services/pm-engine` (Jalon M5, tache 5.5.x) : meme
// convention que `lib/api-server.ts` (token httpOnly ajoute cote serveur
// uniquement, jamais expose au navigateur) — un service Python distinct de
// `atelier-api-server`, mais authentifie de la meme facon (JWT du meme
// fournisseur OIDC, revalide independamment par `pm_engine.auth`).

export interface PendingReview {
  threadId: string;
  repo: string;
  issueNumber: number;
  prUrl: string | null;
  createdAt: string;
}

export class PmEngineError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = "PmEngineError";
  }
}

async function call(path: string, init: RequestInit = {}): Promise<Response> {
  const token = await requireAccessToken();
  const res = await fetch(`${PM_ENGINE_URL}${path}`, {
    ...init,
    headers: {
      ...init.headers,
      Authorization: `Bearer ${token}`,
    },
    cache: "no-store",
  });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new PmEngineError(res.status, body || res.statusText);
  }
  return res;
}

export async function listPendingReviews(): Promise<PendingReview[]> {
  const res = await call("/reviews");
  const rows = (await res.json()) as Array<{
    thread_id: string;
    repo: string;
    issue_number: number;
    pr_url: string | null;
    created_at: string;
  }>;
  return rows.map((r) => ({
    threadId: r.thread_id,
    repo: r.repo,
    issueNumber: r.issue_number,
    prUrl: r.pr_url,
    createdAt: r.created_at,
  }));
}

export async function decideReview(
  threadId: string,
  decision: "approved" | "rejected",
): Promise<{ threadId: string; status: string }> {
  const res = await call(`/reviews/${encodeURIComponent(threadId)}/decision`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ decision }),
  });
  const body = (await res.json()) as { thread_id: string; status: string };
  return { threadId: body.thread_id, status: body.status };
}

export interface ChatMessage {
  role: "user" | "assistant";
  content: string;
}

/**
 * Relaie le flux SSE de `POST /chat` tel quel vers le navigateur (meme
 * principe que `lib/guest-proxy.ts` : le corps de la reponse `fetch` est
 * un `ReadableStream`, retourne directement sans le bufferiser). Le token
 * de session (httpOnly) est ajoute ici, jamais visible cote navigateur.
 * `history` : tours precedents de cette conversation (voir
 * `pm_engine.main::ChatRequest.history` — sans ca, le PM Engine traite
 * chaque message comme une toute premiere conversation).
 */
export async function proxyChat(
  repo: string,
  query: string,
  history: ChatMessage[] = [],
): Promise<Response> {
  const token = await requireAccessToken();
  const upstream = await fetch(`${PM_ENGINE_URL}/chat`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ repo, query, history }),
    cache: "no-store",
  });
  if (!upstream.ok || !upstream.body) {
    const body = await upstream.text().catch(() => "");
    throw new PmEngineError(upstream.status, body || upstream.statusText);
  }
  return new Response(upstream.body, {
    status: upstream.status,
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
      Connection: "keep-alive",
    },
  });
}
