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
