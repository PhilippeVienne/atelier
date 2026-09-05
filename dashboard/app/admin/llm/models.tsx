"use client";

import { useState, useTransition } from "react";
import {
  createLlmModelAction,
  updateLlmModelAction,
  deleteLlmModelAction,
} from "@/app/actions";
import type { LlmModel } from "@/lib/api-server";

// Gestion des modèles LiteLLM (ajout/modification/suppression) — spec
// docs/specs/11-admin-litellm-model-config.md.
//
// `apiKey` ne transite du navigateur au serveur QUE le temps de la requête :
// jamais conservé côté client, jamais réaffiché (même à l'édition, où le
// champ reste vide — le laisser vide PRÉSERVE la clé déjà enregistrée,
// LiteLLM fusionne litellm_params champ par champ plutôt que de le
// remplacer, vérifié empiriquement).
//
// Un modèle sans `id` (entrée statique du `config.yaml`, cas du cluster de
// dev) n'a rien qu'`update`/`delete` puissent cibler : ni éditable ni
// supprimable ici.

function ModelForm({
  title,
  initial,
  onCancel,
  onSubmit,
  requireKey,
}: {
  title: string;
  initial?: Pick<LlmModel, "name" | "target" | "apiBase">;
  onCancel: () => void;
  onSubmit: (formData: FormData) => void;
  requireKey: boolean;
}) {
  return (
    <form action={onSubmit} className="flex flex-col gap-2 rounded-lg bg-surface-hover p-3">
      <div className="grid gap-2 sm:grid-cols-3">
        <label className="flex flex-col gap-1 text-xs">
          <span className="text-muted">Alias exposé</span>
          <input
            name="modelName"
            required
            defaultValue={initial?.name}
            placeholder="claude-3-5-sonnet-20241022"
            className="h-8 rounded border border-border bg-background px-2 font-mono text-sm"
          />
        </label>
        <label className="flex flex-col gap-1 text-xs">
          <span className="text-muted">Modèle réel</span>
          <input
            name="target"
            required
            defaultValue={initial?.target ?? undefined}
            placeholder="anthropic/claude-3-5-sonnet-20241022"
            className="h-8 rounded border border-border bg-background px-2 font-mono text-sm"
          />
        </label>
        <label className="flex flex-col gap-1 text-xs">
          <span className="text-muted">Endpoint (optionnel)</span>
          <input
            name="apiBase"
            defaultValue={initial?.apiBase ?? undefined}
            placeholder="défaut du fournisseur"
            className="h-8 rounded border border-border bg-background px-2 font-mono text-sm"
          />
        </label>
      </div>
      <label className="flex flex-col gap-1 text-xs">
        <span className="text-muted">
          Clé API du fournisseur{" "}
          {!requireKey && (
            <span className="normal-case text-muted">
              (laisser vide pour la conserver)
            </span>
          )}
        </span>
        <input
          name="apiKey"
          type="password"
          required={requireKey}
          autoComplete="off"
          placeholder={requireKey ? "collez la clé" : "••••••••"}
          className="h-8 rounded border border-border bg-background px-2 font-mono text-sm"
        />
      </label>
      <div className="flex items-center justify-between gap-3">
        <span className="text-xs text-muted">
          La clé transite par le serveur jusqu&apos;à LiteLLM. Elle ne sera
          plus jamais affichée.
        </span>
        <div className="flex shrink-0 gap-2">
          <button
            type="button"
            onClick={onCancel}
            className="rounded-full border border-border px-3 py-1 text-sm transition-colors hover:bg-surface-hover"
          >
            Annuler
          </button>
          <button
            type="submit"
            className="rounded-full bg-accent px-3 py-1 text-sm font-medium text-accent-foreground transition-colors hover:bg-accent-hover"
          >
            {title}
          </button>
        </div>
      </div>
    </form>
  );
}

export function Models({ initial }: { initial: LlmModel[] }) {
  const [models, setModels] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [, startTransition] = useTransition();

  function create(formData: FormData) {
    setError(null);
    startTransition(async () => {
      const res = await createLlmModelAction(formData);
      if (res.error || !res.id) {
        setError(res.error ?? "Erreur inconnue.");
        return;
      }
      setModels((prev) => [
        ...prev,
        {
          id: res.id,
          name: String(formData.get("modelName") ?? ""),
          target: String(formData.get("target") ?? ""),
          apiBase: String(formData.get("apiBase") ?? "") || null,
        },
      ]);
      setAdding(false);
    });
  }

  function update(id: string, formData: FormData) {
    setError(null);
    setBusy(id);
    startTransition(async () => {
      const res = await updateLlmModelAction(id, formData);
      if (res?.error) {
        setError(res.error);
      } else {
        setModels((prev) =>
          prev.map((m) =>
            m.id === id
              ? {
                  ...m,
                  name: String(formData.get("modelName") ?? m.name),
                  target: String(formData.get("target") ?? m.target),
                  apiBase: String(formData.get("apiBase") ?? "") || null,
                }
              : m,
          ),
        );
        setEditing(null);
      }
      setBusy(null);
    });
  }

  function remove(id: string) {
    setError(null);
    setBusy(id);
    startTransition(async () => {
      const res = await deleteLlmModelAction(id);
      if (res?.error) setError(res.error);
      else setModels((prev) => prev.filter((m) => m.id !== id));
      setBusy(null);
    });
  }

  return (
    <section className="rounded-xl border border-border bg-surface/70 p-4 flex flex-col gap-3">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-semibold">Modèles</h2>
        <button
          onClick={() => {
            setEditing(null);
            setAdding((v) => !v);
          }}
          className="shrink-0 rounded-full border border-border px-3 py-1 text-sm transition-colors hover:bg-surface-hover"
        >
          {adding ? "Annuler" : "Ajouter"}
        </button>
      </div>

      {adding && (
        <ModelForm title="Ajouter" onCancel={() => setAdding(false)} onSubmit={create} requireKey />
      )}

      {error && <p className="text-sm text-red-600 dark:text-red-400">{error}</p>}

      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-left text-xs uppercase tracking-wide text-muted">
              <th className="pb-2 pr-4 font-normal">Alias</th>
              <th className="pb-2 pr-4 font-normal">Modèle réel</th>
              <th className="pb-2 pr-4 font-normal">Endpoint</th>
              <th className="pb-2 font-normal" />
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {models.map((m) =>
              m.id !== null && editing === m.id ? (
                <tr key={m.name}>
                  <td colSpan={4} className="py-2">
                    <ModelForm
                      title="Enregistrer"
                      initial={m}
                      onCancel={() => setEditing(null)}
                      onSubmit={(fd) => update(m.id!, fd)}
                      requireKey={false}
                    />
                  </td>
                </tr>
              ) : (
                <tr key={m.name}>
                  <td className="py-2 pr-4 font-mono text-xs">{m.name}</td>
                  <td className="py-2 pr-4 font-mono text-xs">{m.target ?? "—"}</td>
                  <td className="py-2 pr-4 font-mono text-xs text-muted break-all">
                    {m.apiBase ?? "défaut du fournisseur"}
                  </td>
                  <td className="py-2 text-right">
                    {m.id ? (
                      <div className="inline-flex gap-2">
                        <button
                          onClick={() => {
                            setAdding(false);
                            setEditing(m.id);
                          }}
                          disabled={busy === m.id}
                          className="rounded-full border border-border px-3 py-1 text-xs transition-colors hover:bg-surface-hover disabled:opacity-50"
                        >
                          Modifier
                        </button>
                        <button
                          onClick={() => remove(m.id!)}
                          disabled={busy === m.id}
                          className="rounded-full border border-red-500/30 px-3 py-1 text-xs text-red-600 transition-colors hover:bg-red-500/10 disabled:opacity-50 dark:text-red-400"
                        >
                          {busy === m.id ? "…" : "Supprimer"}
                        </button>
                      </div>
                    ) : (
                      <span className="text-xs text-muted">config.yaml</span>
                    )}
                  </td>
                </tr>
              ),
            )}
          </tbody>
        </table>
      </div>
    </section>
  );
}
