import "server-only";
import { API_SERVER_URL } from "./config";
import { requireAccessToken } from "./session";

export type WorkshopPhase =
  | "Pending"
  | "BuildingImage"
  | "Provisioning"
  | "Running"
  | "Suspending"
  | "Suspended"
  | "Resuming"
  | "Terminating"
  | "Failed";

export interface DevcontainerSource {
  repo: string;
  revision: string;
  configPath: string;
}

export interface WorkshopResources {
  cpu: string;
  memory: string;
  /** Plafond de depense LLM du Workshop, en dollars. `undefined` = aucun
   *  plafond : le controller n'en pose alors pas sur la Virtual Key
   *  LiteLLM, ce qui n'est PAS la meme chose qu'un plafond a zero. */
  maxLlmBudgetUsd?: number;
  disk?: string | null;
}

export interface WorkshopSpec {
  devcontainer: DevcontainerSource;
  resources: WorkshopResources;
  egressAllowlist: string[];
  tools: string[];
  ownerSubject: string;
  desiredState: "Running" | "Suspended";
}

export interface WorkshopStatus {
  phase: WorkshopPhase;
  podName?: string | null;
  imageDigest?: string | null;
  snapshotDigest?: string | null;
  conditions: Record<string, string>;
}

export interface Workshop {
  metadata: {
    name: string;
    creationTimestamp?: string;
  };
  spec: WorkshopSpec;
  status?: WorkshopStatus;
}

export interface CreateWorkshopInput {
  name: string;
  devcontainer: DevcontainerSource;
  resources: WorkshopResources;
  egressAllowlist?: string[];
  tools?: string[];
}

export class ApiServerError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiServerError";
  }
}

async function call(path: string, init: RequestInit = {}): Promise<Response> {
  const token = await requireAccessToken();
  const res = await fetch(`${API_SERVER_URL}${path}`, {
    ...init,
    headers: {
      ...init.headers,
      Authorization: `Bearer ${token}`,
    },
    // Toujours des donnees live : jamais de cache pour l'etat d'un
    // Workshop (peut changer a tout moment via le controller).
    cache: "no-store",
  });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new ApiServerError(res.status, body || res.statusText);
  }
  return res;
}

export async function listWorkshops(): Promise<Workshop[]> {
  const res = await call("/v1/workshops");
  return res.json();
}

export async function getWorkshop(name: string): Promise<Workshop> {
  const res = await call(`/v1/workshops/${encodeURIComponent(name)}`);
  return res.json();
}

export async function createWorkshop(input: CreateWorkshopInput): Promise<Workshop> {
  const res = await call("/v1/workshops", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
  return res.json();
}

export async function suspendWorkshop(name: string): Promise<Workshop> {
  const res = await call(`/v1/workshops/${encodeURIComponent(name)}/suspend`, { method: "POST" });
  return res.json();
}

export async function resumeWorkshop(name: string): Promise<Workshop> {
  const res = await call(`/v1/workshops/${encodeURIComponent(name)}/resume`, { method: "POST" });
  return res.json();
}

export async function deleteWorkshop(name: string): Promise<void> {
  await call(`/v1/workshops/${encodeURIComponent(name)}`, { method: "DELETE" });
}

export interface WorkshopEvent {
  type: "Normal" | "Warning" | string;
  reason: string;
  message: string;
  involvedObject: string;
  timestamp: string | null;
  count: number;
}

export async function listWorkshopEvents(name: string): Promise<WorkshopEvent[]> {
  const res = await call(`/v1/workshops/${encodeURIComponent(name)}/events`);
  return res.json();
}

export interface LlmBudget {
  spendUsd: number;
  /** `null` = aucun plafond configure, a distinguer d'un plafond nul. */
  maxBudgetUsd: number | null;
  /** Nombre de Virtual Keys trouvees ; `0` = la depense affichee vaut zero
   *  par absence de donnee, pas par mesure. */
  keyCount: number;
}

/** Consommation LLM d'un Workshop, telle que LiteLLM la comptabilise sur ses
 *  Virtual Keys. Renvoie `null` si la passerelle n'est pas configuree ou
 *  n'est pas joignable (503) : c'est une information d'appoint, son absence
 *  ne doit pas empecher d'afficher le Workshop. */
export async function getLlmBudget(name: string): Promise<LlmBudget | null> {
  try {
    const res = await call(`/v1/workshops/${encodeURIComponent(name)}/llm-budget`);
    return (await res.json()) as LlmBudget;
  } catch (err) {
    if (err instanceof ApiServerError && err.status === 503) return null;
    throw err;
  }
}

export interface LlmModel {
  name: string;
  target: string | null;
  apiBase: string | null;
}

export interface LlmKey {
  alias: string;
  owner: string | null;
  spendUsd: number;
  maxBudgetUsd: number | null;
  expiresAt: string | null;
  expired: boolean;
}

export interface LlmOverview {
  globalSpendUsd: number | null;
  models: LlmModel[];
  keys: LlmKey[];
}

/** Vue d'administration de la passerelle LiteLLM. Reservee au role `admin`,
 *  ce que l'api-server verifie lui-meme (`403` sinon). */
export async function getLlmOverview(): Promise<LlmOverview> {
  const res = await call("/v1/admin/llm");
  return (await res.json()) as LlmOverview;
}

export interface Credential {
  host: string;
  header: string;
  prefix: string;
  /** Chemin OpenBao : dit où le secret vit, jamais ce qu'il vaut. */
  secretPath: string;
}

export async function listCredentials(name: string): Promise<Credential[]> {
  const res = await call(`/v1/workshops/${encodeURIComponent(name)}/credentials`);
  return (await res.json()) as Credential[];
}

/** Enregistre un credential. La valeur ne fait que TRAVERSER le serveur, qui
 *  la dépose dans OpenBao — elle n'est ni stockée ici, ni relisible ensuite,
 *  y compris par l'api-server (sa policy OpenBao ne lui accorde pas `read`). */
export async function putCredential(
  name: string,
  input: { host: string; header: string; prefix: string; value: string },
): Promise<Credential> {
  const res = await call(`/v1/workshops/${encodeURIComponent(name)}/credentials`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
  return (await res.json()) as Credential;
}

export async function deleteCredential(name: string, host: string): Promise<void> {
  await call(
    `/v1/workshops/${encodeURIComponent(name)}/credentials/${encodeURIComponent(host)}`,
    { method: "DELETE" },
  );
}
