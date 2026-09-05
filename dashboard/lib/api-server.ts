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

/** Port applicatif expose aux autres Workshops d'une meme campagne (tache
 *  12.1/12.6, spec docs/specs/16-escouades-multi-agents-swarms-mesh.md
 *  §3.2). */
export interface ExportedService {
  name: string;
  port: number;
}

export interface WorkshopSpec {
  devcontainer: DevcontainerSource;
  resources: WorkshopResources;
  egressAllowlist: string[];
  tools: string[];
  ownerSubject: string;
  desiredState: "Running" | "Suspended";
  /** Escouades multi-Workshops (tache 12.6) : `null`/absent du CRD = Workshop
   *  solitaire, exclu de la vue Campagnes. Toujours PRESENT dans la reponse
   *  JSON de l'api-server (`Option<String>` cote Rust, jamais omis a la
   *  serialisation — contrairement a `podName`/`imageDigest` sur
   *  `WorkshopStatus`, qui eux le sont). */
  campaignId: string | null;
  /** Toujours present (`Vec<T>` cote Rust, jamais `Option`) : `[]` pour un
   *  Workshop qui n'exporte rien. */
  exportedServices: ExportedService[];
  /** Format `<service>.<workshop-cible>.atelier.internal:<port>` — voir
   *  `crates/net-proxy/src/internal.rs`, table `squad`. Toujours present,
   *  meme convention que `exportedServices`. */
  allowedInternalTargets: string[];
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
  /** `null` pour une entrée statique du `config.yaml` (cas du cluster de
   *  dev) : sans identifiant, `updateLlmModel`/`deleteLlmModel` n'ont rien à
   *  cibler — seuls les modèles ajoutés dynamiquement en ont un. */
  id: string | null;
  name: string;
  target: string | null;
  apiBase: string | null;
}

export interface LlmModelInput {
  modelName: string;
  target: string;
  apiBase?: string;
  /** Absente sur une modification : LiteLLM fusionne `litellm_params` champ
   *  par champ et préserve alors la clé déjà enregistrée (vérifié
   *  empiriquement, voir `docs/specs/11-admin-litellm-model-config.md` §5)
   *  — obligatoire à la création. */
  apiKey?: string;
}

/** Ajoute un modèle/provider à la passerelle LiteLLM. Réservé au rôle
 *  `admin`, vérifié côté api-server. */
export async function createLlmModel(input: LlmModelInput): Promise<{ id: string }> {
  const res = await call("/v1/admin/llm/models", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
  return (await res.json()) as { id: string };
}

/** Modifie un modèle existant. `input.apiKey` omis = clé conservée. */
export async function updateLlmModel(id: string, input: LlmModelInput): Promise<void> {
  await call(`/v1/admin/llm/models/${encodeURIComponent(id)}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
}

export async function deleteLlmModel(id: string): Promise<void> {
  await call(`/v1/admin/llm/models/${encodeURIComponent(id)}`, { method: "DELETE" });
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

export interface SpendBucket {
  label: string;
  spendUsd: number;
  requestCount: number;
}

export interface SpendReport {
  totalUsd: number;
  testPricingUsd: number;
  unattributedUsd: number;
  byDay: SpendBucket[];
  byGroup: SpendBucket[];
  byModel: SpendBucket[];
}

/** Rapport de dépense agrégé depuis les journaux LiteLLM. `null` si la
 *  passerelle est injoignable : une console partielle vaut mieux qu'une
 *  console qui refuse de s'afficher. */
export async function getSpendReport(): Promise<SpendReport | null> {
  try {
    const res = await call("/v1/admin/llm/spend");
    return (await res.json()) as SpendReport;
  } catch (err) {
    if (err instanceof ApiServerError && err.status === 503) return null;
    throw err;
  }
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

/** Demande d'approbation Human-in-the-Loop (spec
 *  `docs/specs/14-devex-cli-simulateurs-hitl.md` §5, tache 9.5/9.6) — memes
 *  champs que `crates/api-server/src/approvals.rs::HitlRequest`, en
 *  camelCase (serde `rename_all`). */
export interface HitlRequest {
  id: string;
  tenant: string;
  workshopName: string;
  category: "ALLOWLIST_EXPANSION" | "SECRET_REQUEST" | "PR_GATEWAY" | "SHELL_COMMAND";
  requestedBy: string;
  payload: unknown;
  status: "PENDING" | "APPROVED" | "REJECTED" | "EXPIRED";
  decidedBy: string | null;
  decisionReason: string | null;
  createdAt: string;
  expiresAt: string;
  decidedAt: string | null;
}

export async function listApprovals(workshopName: string): Promise<HitlRequest[]> {
  const res = await call(`/v1/workshops/${encodeURIComponent(workshopName)}/approvals`);
  return (await res.json()) as HitlRequest[];
}

export async function decideApproval(
  id: string,
  decision: "APPROVED" | "REJECTED",
  reason?: string,
): Promise<HitlRequest> {
  const res = await call(`/v1/approvals/${encodeURIComponent(id)}/decision`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ decision, reason }),
  });
  return (await res.json()) as HitlRequest;
}
