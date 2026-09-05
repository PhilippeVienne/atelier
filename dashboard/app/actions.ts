"use server";

import { revalidatePath } from "next/cache";
import { redirect } from "next/navigation";
import {
  ApiServerError,
  createWorkshop,
  deleteWorkshop,
  resumeWorkshop,
  suspendWorkshop,
  putCredential,
  deleteCredential,
  createLlmModel,
  updateLlmModel,
  deleteLlmModel,
  decideApproval,
  type LlmModelInput,
} from "@/lib/api-server";
import { decideReview } from "@/lib/pm-engine";
import { destroySession } from "@/lib/session";
import { createMirrorProject, ForgejoError } from "@/lib/forgejo";

export async function logout() {
  await destroySession();
  redirect("/login");
}

export async function suspend(name: string) {
  await suspendWorkshop(name);
  revalidatePath("/workshops");
}

export async function resume(name: string) {
  await resumeWorkshop(name);
  revalidatePath("/workshops");
}

export async function remove(name: string) {
  await deleteWorkshop(name);
  revalidatePath("/workshops");
}

export async function decideReviewAction(threadId: string, decision: "approved" | "rejected") {
  await decideReview(threadId, decision);
  revalidatePath("/pm");
}

/** Distinct de `decideReviewAction` ci-dessus : celle-ci decide une revue
 *  PM Engine (`lib/pm-engine`), pas une demande HITL (`hitl_requests`,
 *  tache 9.5/9.6) — deux mecanismes d'approbation independants. */
export async function decideApprovalAction(
  workshopName: string,
  id: string,
  decision: "APPROVED" | "REJECTED",
  reason?: string,
) {
  try {
    const updated = await decideApproval(id, decision, reason);
    revalidatePath(`/workshops/${workshopName}`);
    return { request: updated };
  } catch (err) {
    if (err instanceof ApiServerError) {
      return { error: `Décision refusée (${err.status}).` };
    }
    throw err;
  }
}

export interface CreateWorkshopState {
  error?: string;
}

export async function createWorkshopAction(
  _prevState: CreateWorkshopState,
  formData: FormData,
): Promise<CreateWorkshopState> {
  const name = String(formData.get("name") ?? "").trim();
  const repo = String(formData.get("repo") ?? "").trim();
  const revision = String(formData.get("revision") ?? "").trim() || "HEAD";
  const configPath =
    String(formData.get("configPath") ?? "").trim() || ".devcontainer/devcontainer.json";
  const cpu = String(formData.get("cpu") ?? "").trim() || "1";
  const memory = String(formData.get("memory") ?? "").trim() || "512Mi";
  // Champ laisse vide = pas de plafond, et surtout pas un plafond a zero :
  // `Number("")` vaut `0`, ce qui couperait tout appel LLM des le premier.
  const budgetRaw = String(formData.get("maxLlmBudgetUsd") ?? "").trim();
  const maxLlmBudgetUsd = budgetRaw === "" ? undefined : Number(budgetRaw);
  if (maxLlmBudgetUsd !== undefined && !Number.isFinite(maxLlmBudgetUsd)) {
    return { error: "Le budget LLM doit etre un nombre." };
  }
  const egressAllowlist = String(formData.get("egressAllowlist") ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);

  if (!name || !repo) {
    return { error: "nom et depot devcontainer requis" };
  }

  try {
    await createWorkshop({
      name,
      devcontainer: { repo, revision, configPath },
      resources: { cpu, memory, maxLlmBudgetUsd },
      egressAllowlist,
    });
  } catch (err) {
    const message = err instanceof ApiServerError ? err.message : "erreur inattendue";
    return { error: message };
  }

  revalidatePath("/workshops");
  redirect("/workshops");
}

export interface CreateMirrorProjectState {
  error?: string;
}

export async function createMirrorProjectAction(
  _prevState: CreateMirrorProjectState,
  formData: FormData,
): Promise<CreateMirrorProjectState> {
  const name = String(formData.get("name") ?? "").trim();
  const sourceUrl = String(formData.get("sourceUrl") ?? "").trim();
  const isPrivate = formData.get("private") === "on";
  const token = String(formData.get("token") ?? "").trim() || undefined;

  if (!name || !sourceUrl) {
    return { error: "nom et URL source requis" };
  }
  if (isPrivate && !token) {
    return { error: "un jeton d'acces est requis pour un depot prive" };
  }

  try {
    await createMirrorProject({ name, sourceUrl, private: isPrivate, token });
  } catch (err) {
    const message = err instanceof ForgejoError ? err.message : "erreur inattendue";
    return { error: message };
  }

  revalidatePath("/projects");
  redirect("/projects");
}

/** Enregistre un credential pour un Workshop.
 *
 * La valeur est envoyée par le navigateur mais l'écriture se fait ICI, côté
 * serveur : elle traverse la Server Action puis l'api-server jusqu'à
 * OpenBao, sans jamais être conservée ni relue. Ni le navigateur ni cette
 * application n'en gardent trace.
 */
export async function putCredentialAction(name: string, formData: FormData) {
  const host = String(formData.get("host") ?? "").trim();
  const header = String(formData.get("header") ?? "").trim() || "Authorization";
  const prefix = String(formData.get("prefix") ?? "");
  const value = String(formData.get("value") ?? "");
  if (!host || !value) {
    return { error: "L'hôte et la valeur sont requis." };
  }
  try {
    await putCredential(name, { host, header, prefix, value });
  } catch (err) {
    // Le message brut de l'api-server peut citer le chemin OpenBao visé :
    // inutile de le remonter jusqu'au navigateur.
    if (err instanceof ApiServerError) {
      return { error: `Enregistrement refusé (${err.status}).` };
    }
    throw err;
  }
  revalidatePath(`/workshops/${name}`);
  return {};
}

export async function deleteCredentialAction(name: string, host: string) {
  try {
    await deleteCredential(name, host);
  } catch (err) {
    if (err instanceof ApiServerError) {
      return { error: `Suppression refusée (${err.status}).` };
    }
    throw err;
  }
  revalidatePath(`/workshops/${name}`);
  return {};
}

function llmModelInputFrom(formData: FormData): LlmModelInput {
  const modelName = String(formData.get("modelName") ?? "").trim();
  const target = String(formData.get("target") ?? "").trim();
  const apiBase = String(formData.get("apiBase") ?? "").trim();
  const apiKey = String(formData.get("apiKey") ?? "");
  return {
    modelName,
    target,
    apiBase: apiBase || undefined,
    // Champ vide = absent, jamais une chaine vide envoyee a l'api-server :
    // c'est cette absence qui signale "conserver la cle actuelle" a l'edition
    // (voir `LlmBudgetClient::update_model`).
    apiKey: apiKey || undefined,
  };
}

/** Ajoute un modèle LiteLLM (spec docs/specs/11-admin-litellm-model-config.md).
 *  Réservé au rôle `admin`, vérifié par l'api-server (403 sinon). */
export async function createLlmModelAction(formData: FormData) {
  const input = llmModelInputFrom(formData);
  if (!input.modelName || !input.target || !input.apiKey) {
    return { error: "Alias, modèle réel et clé API sont requis." };
  }
  try {
    const { id } = await createLlmModel(input);
    revalidatePath("/admin/llm");
    return { id, error: undefined };
  } catch (err) {
    if (err instanceof ApiServerError) {
      return { error: `Création refusée (${err.status}).`, id: undefined };
    }
    throw err;
  }
}

export async function updateLlmModelAction(id: string, formData: FormData) {
  const input = llmModelInputFrom(formData);
  if (!input.modelName || !input.target) {
    return { error: "Alias et modèle réel sont requis." };
  }
  try {
    await updateLlmModel(id, input);
  } catch (err) {
    if (err instanceof ApiServerError) {
      return { error: `Modification refusée (${err.status}).` };
    }
    throw err;
  }
  revalidatePath("/admin/llm");
  return {};
}

export async function deleteLlmModelAction(id: string) {
  try {
    await deleteLlmModel(id);
  } catch (err) {
    if (err instanceof ApiServerError) {
      return { error: `Suppression refusée (${err.status}).` };
    }
    throw err;
  }
  revalidatePath("/admin/llm");
  return {};
}
