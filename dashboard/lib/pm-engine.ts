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

export interface ChatHistoryToolCall {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  result: Record<string, unknown>;
}

export interface ChatHistoryEntry extends ChatMessage {
  toolCalls: ChatHistoryToolCall[];
}

/**
 * Historique persiste des tours de chat PM de l'utilisateur courant pour
 * `repo` (`GET /chat/history` cote pm-engine, tache 5.5.1) : sans lui, la
 * conversation de `PmChat` (`useState` pur) disparaissait a chaque
 * rechargement de page. `toolCalls` inclus (tache suivante, "elements
 * interactifs") : sans ca, la carte d'appel d'outil affichee en direct
 * disparaissait elle aussi au rechargement alors que le texte final qui la
 * suit restait visible.
 */
export async function fetchChatHistory(repo: string): Promise<ChatHistoryEntry[]> {
  const res = await call(`/chat/history?repo=${encodeURIComponent(repo)}`);
  const rows = (await res.json()) as Array<{
    role: "user" | "assistant";
    content: string;
    tool_calls: ChatHistoryToolCall[];
  }>;
  return rows.map((r) => ({ role: r.role, content: r.content, toolCalls: r.tool_calls }));
}

// --------------------------------------------------------------------------
// Workflows (« mission control », suivi du pipeline PM en direct)
// --------------------------------------------------------------------------

export interface WorkflowSubTask {
  id: string;
  title: string;
  scope: string[];
  workshopName: string;
  branchName: string;
}

export interface WorkflowWorkshop {
  name: string;
  phase: string | null;
  podName: string | null;
}

export interface WorkflowState {
  threadId: string;
  /** Date du premier checkpoint = départ réel du workflow (ISO 8601). */
  startedAt: string | null;
  /** Date du dernier checkpoint : donne la durée d'un run terminé. */
  updatedAt: string | null;
  workshops: WorkflowWorkshop[];
  repo: string | null;
  issueNumber: number | null;
  issueTitle: string | null;
  issueUrl: string | null;
  phase: string | null;
  phaseIndex: number;
  phases: string[];
  pendingNodes: string[];
  plan: WorkflowSubTask[];
  correctionAttempts: number;
  maxCorrectionAttempts: number;
  testPassed: boolean | null;
  testOutput: string | null;
  integrationConflicts: string[];
  prNumber: number | null;
  prUrl: string | null;
  prChangedFiles: number | null;
  status: string | null;
}

/** Le `thread_id` vaut `owner/repo#42` : il contient une barre oblique et un
 *  `#`, donc il traverse l'URL en segments encodes cote pm-engine
 *  (`{thread_id:path}`). On encode ici chaque segment separement pour que la
 *  barre reste une barre et que le `#` ne soit pas pris pour un fragment. */
function threadPath(threadId: string): string {
  return threadId.split("/").map(encodeURIComponent).join("/");
}

export async function getWorkflow(threadId: string): Promise<WorkflowState> {
  const res = await call(`/workflows/${threadPath(threadId)}`);
  const w = (await res.json()) as Record<string, never> & {
    thread_id: string;
    started_at: string | null;
    updated_at: string | null;
    workshops: Array<{ name: string; phase: string | null; pod_name: string | null }>;
    repo: string | null;
    issue_number: number | null;
    issue_title: string | null;
    issue_url: string | null;
    phase: string | null;
    phase_index: number;
    phases: string[];
    pending_nodes: string[];
    plan: Array<{
      id: string;
      title: string;
      scope: string[];
      workshop_name: string;
      branch_name: string;
    }>;
    correction_attempts: number;
    max_correction_attempts: number;
    test_passed: boolean | null;
    test_output: string | null;
    integration_conflicts: string[];
    pr_number: number | null;
    pr_url: string | null;
    pr_changed_files: number | null;
    status: string | null;
  };
  return {
    threadId: w.thread_id,
    startedAt: w.started_at,
    updatedAt: w.updated_at,
    workshops: w.workshops.map((k) => ({
      name: k.name,
      phase: k.phase,
      podName: k.pod_name,
    })),
    repo: w.repo,
    issueNumber: w.issue_number,
    issueTitle: w.issue_title,
    issueUrl: w.issue_url,
    phase: w.phase,
    phaseIndex: w.phase_index,
    phases: w.phases,
    pendingNodes: w.pending_nodes,
    plan: w.plan.map((t) => ({
      id: t.id,
      title: t.title,
      scope: t.scope,
      workshopName: t.workshop_name,
      branchName: t.branch_name,
    })),
    correctionAttempts: w.correction_attempts,
    maxCorrectionAttempts: w.max_correction_attempts,
    testPassed: w.test_passed,
    testOutput: w.test_output,
    integrationConflicts: w.integration_conflicts,
    prNumber: w.pr_number,
    prUrl: w.pr_url,
    prChangedFiles: w.pr_changed_files,
    status: w.status,
  };
}

/** `devcontainerRepo` est optionnel : sans lui, le pm-engine deduit l'URL de
 *  clone de son propre gabarit de deploiement
 *  (`PM_ENGINE_DEVCONTAINER_REPO_TEMPLATE`). C'est le chemin normal — ce
 *  gabarit peut porter des identifiants, qui n'ont alors aucune raison de
 *  passer par le navigateur. */
export async function launchWorkflow(
  repo: string,
  issueNumber: number,
  devcontainerRepo?: string,
): Promise<{ threadId: string }> {
  const res = await call("/workflows", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      repo,
      issue_number: issueNumber,
      ...(devcontainerRepo ? { devcontainer_repo: devcontainerRepo } : {}),
    }),
  });
  const body = (await res.json()) as { thread_id: string };
  return { threadId: body.thread_id };
}

export interface WorkflowSummary {
  threadId: string;
  repo: string;
  issueNumber: number;
  issueTitle: string | null;
  phase: string | null;
  phaseIndex: number;
  prUrl: string | null;
  testPassed: boolean | null;
}

export async function listWorkflows(): Promise<WorkflowSummary[]> {
  const res = await call("/workflows");
  const rows = (await res.json()) as Array<{
    thread_id: string;
    repo: string;
    issue_number: number;
    issue_title: string | null;
    phase: string | null;
    phase_index: number;
    pr_url: string | null;
    test_passed: boolean | null;
  }>;
  return rows.map((r) => ({
    threadId: r.thread_id,
    repo: r.repo,
    issueNumber: r.issue_number,
    issueTitle: r.issue_title,
    phase: r.phase,
    phaseIndex: r.phase_index,
    prUrl: r.pr_url,
    testPassed: r.test_passed,
  }));
}
