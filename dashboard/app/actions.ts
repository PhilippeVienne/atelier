"use server";

import { revalidatePath } from "next/cache";
import { redirect } from "next/navigation";
import {
  ApiServerError,
  createWorkshop,
  deleteWorkshop,
  resumeWorkshop,
  suspendWorkshop,
} from "@/lib/api-server";
import { destroySession } from "@/lib/session";

export async function logout() {
  await destroySession();
  redirect("/login");
}

export async function suspend(name: string) {
  await suspendWorkshop(name);
  revalidatePath("/");
}

export async function resume(name: string) {
  await resumeWorkshop(name);
  revalidatePath("/");
}

export async function remove(name: string) {
  await deleteWorkshop(name);
  revalidatePath("/");
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
      resources: { cpu, memory },
      egressAllowlist,
    });
  } catch (err) {
    const message = err instanceof ApiServerError ? err.message : "erreur inattendue";
    return { error: message };
  }

  revalidatePath("/");
  redirect("/");
}
