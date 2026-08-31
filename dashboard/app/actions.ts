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
