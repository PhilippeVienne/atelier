"use client";

import { useRouter } from "next/navigation";
import { useState } from "react";

// Lancement d'un workflow PM depuis la console : choisir un projet, donner
// un numero de ticket, et suivre le pipeline.
//
// L'URL de clone vue par les guests n'est PAS demandee ici : le pm-engine la
// deduit de son gabarit de deploiement. Elle depend de la topologie reseau du
// cluster (les microVM n'ont ni le DNS ni le `/etc/hosts` de l'hote) et peut
// porter des identifiants — deux raisons de ne pas la faire transiter par le
// navigateur ni de la demander a l'utilisateur.
export function Launcher({ projects }: { projects: string[] }) {
  const router = useRouter();
  const [repo, setRepo] = useState(projects[0] ?? "");
  const [issue, setIssue] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const issueNumber = Number.parseInt(issue, 10);
  const canLaunch =
    repo.trim().length > 0 && Number.isInteger(issueNumber) && issueNumber > 0 && !pending;

  async function launch() {
    setPending(true);
    setError(null);
    try {
      const res = await fetch("/api/pm/workflows", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ repo, issueNumber }),
      });
      const body = (await res.json()) as { threadId?: string; message?: string };
      if (!res.ok || !body.threadId) throw new Error(body.message ?? `HTTP ${res.status}`);
      const path = body.threadId.split("/").map(encodeURIComponent).join("/");
      router.push(`/pipeline/${path}`);
    } catch (err) {
      setError(err instanceof Error ? err.message : "echec du lancement");
      setPending(false);
    }
  }

  return (
    <div className="rounded-xl border border-border bg-surface/70 backdrop-blur p-5">
      <h2 className="text-sm font-semibold">Lancer un ticket</h2>
      <p className="mt-1 text-xs text-muted">
        Le PM analyse le ticket, le decoupe, provisionne une microVM par
        sous-tache, delegue a Claude Code, integre, teste et ouvre la Pull
        Request.
      </p>
      <div className="mt-4 flex flex-wrap items-end gap-3">
        <label className="flex flex-col gap-1">
          <span className="text-xs text-muted">Projet</span>
          {projects.length > 0 ? (
            <select
              value={repo}
              onChange={(e) => setRepo(e.target.value)}
              className="h-9 rounded-lg border border-border bg-surface px-3 text-sm min-w-56"
            >
              {projects.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          ) : (
            <input
              value={repo}
              onChange={(e) => setRepo(e.target.value)}
              placeholder="proprietaire/depot"
              className="h-9 rounded-lg border border-border bg-surface px-3 text-sm min-w-56"
            />
          )}
        </label>
        <label className="flex flex-col gap-1">
          <span className="text-xs text-muted">Ticket</span>
          <input
            value={issue}
            onChange={(e) => setIssue(e.target.value)}
            inputMode="numeric"
            placeholder="16"
            className="h-9 w-24 rounded-lg border border-border bg-surface px-3 text-sm"
          />
        </label>
        <button
          onClick={launch}
          disabled={!canLaunch}
          className="h-9 rounded-lg bg-accent px-4 text-sm font-medium text-accent-foreground transition-colors hover:bg-accent-hover disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {pending ? "Demarrage…" : "Lancer"}
        </button>
      </div>
      {error && <p className="mt-3 text-xs text-red-600 dark:text-red-400">{error}</p>}
    </div>
  );
}
