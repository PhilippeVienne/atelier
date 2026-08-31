"use client";

import { useState, useTransition } from "react";
import { putCredentialAction, deleteCredentialAction } from "@/app/actions";
import type { Credential } from "@/lib/api-server";

// Credentials d'un Workshop : règles d'injection `identity-proxy`.
//
// La valeur saisie ici part vers une Server Action et n'est jamais conservée
// côté navigateur — pas d'état React qui la retiendrait après l'envoi, et le
// champ est vidé dès le succès. Elle finit dans OpenBao, d'où même
// l'api-server ne peut pas la relire.

export function Credentials({
  workshopName,
  initial,
}: {
  workshopName: string;
  initial: Credential[];
}) {
  const [credentials, setCredentials] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [, startTransition] = useTransition();
  const [open, setOpen] = useState(false);

  function submit(formData: FormData) {
    const host = String(formData.get("host") ?? "").trim();
    setError(null);
    setBusy(host);
    startTransition(async () => {
      const res = await putCredentialAction(workshopName, formData);
      if (res?.error) {
        setError(res.error);
      } else {
        setCredentials((prev) => [
          ...prev.filter((c) => c.host !== host),
          {
            host,
            header: String(formData.get("header") ?? "") || "Authorization",
            prefix: String(formData.get("prefix") ?? ""),
            secretPath: `credentials/${host.toLowerCase()}`,
          },
        ]);
        setOpen(false);
      }
      setBusy(null);
    });
  }

  function remove(host: string) {
    setError(null);
    setBusy(host);
    startTransition(async () => {
      const res = await deleteCredentialAction(workshopName, host);
      if (res?.error) setError(res.error);
      else setCredentials((prev) => prev.filter((c) => c.host !== host));
      setBusy(null);
    });
  }

  return (
    <div className="rounded-xl border border-border bg-surface p-5 shadow-sm flex flex-col gap-3">
      <div className="flex items-center justify-between gap-3">
        <div>
          <p className="text-xs uppercase tracking-wide text-muted">Credentials</p>
          <p className="mt-1 text-xs text-muted">
            L&apos;agent appelle l&apos;API sans jamais détenir le secret :
            c&apos;est le proxy qui pose l&apos;en-tête au passage.
          </p>
        </div>
        <button
          onClick={() => setOpen((v) => !v)}
          className="shrink-0 rounded-full border border-border px-3 py-1 text-sm transition-colors hover:bg-surface-hover"
        >
          {open ? "Annuler" : "Ajouter"}
        </button>
      </div>

      {open && (
        <form action={submit} className="flex flex-col gap-2 rounded-lg bg-surface-hover p-3">
          <div className="grid gap-2 sm:grid-cols-3">
            <label className="flex flex-col gap-1 text-xs">
              <span className="text-muted">Hôte</span>
              <input
                name="host"
                required
                placeholder="api.exemple.com"
                className="h-8 rounded border border-border bg-background px-2 text-sm"
              />
            </label>
            <label className="flex flex-col gap-1 text-xs">
              <span className="text-muted">En-tête</span>
              <input
                name="header"
                defaultValue="Authorization"
                className="h-8 rounded border border-border bg-background px-2 text-sm"
              />
            </label>
            <label className="flex flex-col gap-1 text-xs">
              <span className="text-muted">Préfixe</span>
              <input
                name="prefix"
                defaultValue="Bearer "
                className="h-8 rounded border border-border bg-background px-2 font-mono text-sm"
              />
            </label>
          </div>
          <label className="flex flex-col gap-1 text-xs">
            <span className="text-muted">Valeur</span>
            {/* `type="password"` et `autoComplete="off"` : ni affichage en
                clair a l'ecran, ni enregistrement par le gestionnaire de mots
                de passe du navigateur — ce secret appartient au Workshop, pas
                a la personne qui le saisit. */}
            <input
              name="value"
              type="password"
              required
              autoComplete="off"
              placeholder="collez le jeton"
              className="h-8 rounded border border-border bg-background px-2 font-mono text-sm"
            />
          </label>
          <div className="flex items-center justify-between gap-3">
            <span className="text-xs text-muted">
              La valeur transite par le serveur et va dans OpenBao. Elle ne sera
              plus jamais affichée.
            </span>
            <button
              type="submit"
              className="shrink-0 rounded-full bg-accent px-3 py-1 text-sm font-medium text-accent-foreground transition-colors hover:bg-accent-hover"
            >
              Enregistrer
            </button>
          </div>
        </form>
      )}

      {error && <p className="text-sm text-red-600 dark:text-red-400">{error}</p>}

      {credentials.length === 0 ? (
        <p className="text-sm text-muted">Aucun credential.</p>
      ) : (
        <ul className="flex flex-col divide-y divide-border">
          {credentials.map((c) => (
            <li key={c.host} className="flex items-center justify-between gap-3 py-2">
              <div className="min-w-0">
                <p className="truncate font-mono text-sm">{c.host}</p>
                <p className="truncate text-xs text-muted">
                  {c.header}: {c.prefix}
                  <span className="italic">••••••</span>
                </p>
              </div>
              <button
                onClick={() => remove(c.host)}
                disabled={busy === c.host}
                className="shrink-0 rounded-full border border-red-500/30 px-3 py-1 text-xs text-red-600 transition-colors hover:bg-red-500/10 disabled:opacity-50 dark:text-red-400"
              >
                {busy === c.host ? "…" : "Supprimer"}
              </button>
            </li>
          ))}
        </ul>
      )}

      {credentials.length > 0 && (
        <p className="text-xs text-muted">
          Les règles sont relues périodiquement par <code>identity-proxy</code>
          {" "}: un ajout devient actif en quelques minutes, sans redémarrer le
          Workshop.
        </p>
      )}
    </div>
  );
}
