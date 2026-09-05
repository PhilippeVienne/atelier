"use client";

import { useState, useTransition } from "react";
import { decideApprovalAction } from "@/app/actions";
import type { HitlRequest } from "@/lib/api-server";

// Demandes d'approbation Human-in-the-Loop d'un Workshop (tache 9.6, spec
// docs/specs/14-devex-cli-simulateurs-hitl.md §5.4) : bandeau des demandes
// PENDING en attente d'un humain du groupe proprietaire, historique des
// demandes deja tranchees en dessous.

const CATEGORY_LABELS: Record<HitlRequest["category"], string> = {
  ALLOWLIST_EXPANSION: "Extension d'allowlist",
  SECRET_REQUEST: "Demande de secret",
  PR_GATEWAY: "Validation de Pull Request",
  SHELL_COMMAND: "Commande shell",
};

const STATUS_LABELS: Record<HitlRequest["status"], string> = {
  PENDING: "En attente",
  APPROVED: "Approuvée",
  REJECTED: "Rejetée",
  EXPIRED: "Expirée",
};

function StatusBadge({ status }: { status: HitlRequest["status"] }) {
  const colors: Record<HitlRequest["status"], string> = {
    PENDING: "bg-amber-500/15 text-amber-600 dark:text-amber-400",
    APPROVED: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
    REJECTED: "bg-red-500/15 text-red-600 dark:text-red-400",
    EXPIRED: "bg-muted/20 text-muted",
  };
  return (
    <span className={`shrink-0 rounded-full px-2 py-0.5 text-xs font-medium ${colors[status]}`}>
      {STATUS_LABELS[status]}
    </span>
  );
}

export function Approvals({
  workshopName,
  initial,
}: {
  workshopName: string;
  initial: HitlRequest[];
}) {
  const [requests, setRequests] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [, startTransition] = useTransition();

  function decide(id: string, decision: "APPROVED" | "REJECTED") {
    setError(null);
    setBusy(id);
    const reason =
      decision === "REJECTED" ? window.prompt("Motif du rejet (optionnel)") ?? undefined : undefined;
    startTransition(async () => {
      const res = await decideApprovalAction(workshopName, id, decision, reason);
      if (res.error) {
        setError(res.error);
      } else if (res.request) {
        setRequests((prev) => prev.map((r) => (r.id === id ? res.request! : r)));
      }
      setBusy(null);
    });
  }

  const pending = requests.filter((r) => r.status === "PENDING");
  const decided = requests.filter((r) => r.status !== "PENDING");

  return (
    <div className="rounded-xl border border-border bg-surface p-5 shadow-sm flex flex-col gap-3">
      <div>
        <p className="text-xs uppercase tracking-wide text-muted">Approbations (HITL)</p>
        <p className="mt-1 text-xs text-muted">
          Actions sensibles demandées par l&apos;agent, en attente d&apos;une
          décision humaine — expirent automatiquement après 15 minutes sans
          réponse.
        </p>
      </div>

      {error && <p className="text-sm text-red-600 dark:text-red-400">{error}</p>}

      {pending.length === 0 ? (
        <p className="text-sm text-muted">Aucune demande en attente.</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {pending.map((r) => (
            <li
              key={r.id}
              className="flex flex-col gap-2 rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 sm:flex-row sm:items-center sm:justify-between"
            >
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-medium">{CATEGORY_LABELS[r.category]}</span>
                  <StatusBadge status={r.status} />
                </div>
                <p className="mt-1 truncate font-mono text-xs text-muted">
                  {JSON.stringify(r.payload)}
                </p>
                <p className="text-xs text-muted">
                  Demandé par {r.requestedBy} · expire {new Date(r.expiresAt).toLocaleTimeString("fr-FR")}
                </p>
              </div>
              <div className="flex shrink-0 gap-2">
                <button
                  onClick={() => decide(r.id, "APPROVED")}
                  disabled={busy === r.id}
                  className="rounded-full bg-accent px-3 py-1 text-xs font-medium text-accent-foreground transition-colors hover:bg-accent-hover disabled:opacity-50"
                >
                  {busy === r.id ? "…" : "Approuver"}
                </button>
                <button
                  onClick={() => decide(r.id, "REJECTED")}
                  disabled={busy === r.id}
                  className="rounded-full border border-red-500/30 px-3 py-1 text-xs text-red-600 transition-colors hover:bg-red-500/10 disabled:opacity-50 dark:text-red-400"
                >
                  {busy === r.id ? "…" : "Rejeter"}
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}

      {decided.length > 0 && (
        <details className="text-xs text-muted">
          <summary className="cursor-pointer select-none">
            Historique ({decided.length})
          </summary>
          <ul className="mt-2 flex flex-col divide-y divide-border">
            {decided.map((r) => (
              <li key={r.id} className="flex items-center justify-between gap-3 py-2">
                <div className="min-w-0">
                  <p className="truncate">{CATEGORY_LABELS[r.category]}</p>
                  {r.decisionReason && (
                    <p className="truncate italic">« {r.decisionReason} »</p>
                  )}
                </div>
                <StatusBadge status={r.status} />
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
