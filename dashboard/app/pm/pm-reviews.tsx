"use client";

import Link from "next/link";
import { useState, useTransition } from "react";
import { decideReviewAction } from "@/app/actions";
import type { PendingReview } from "@/lib/pm-engine";

/** Date lisible et absolue : « il y a 3 h » oblige a recalculer mentalement
 *  quand on compare deux revues, alors qu'une PR en attente se juge sur son
 *  age reel. */
function formatDate(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime())
    ? ""
    : d.toLocaleString("fr-FR", {
        day: "2-digit",
        month: "short",
        hour: "2-digit",
        minute: "2-digit",
      });
}

// Tache 5.5.2 : approuve/rejette une PR ouverte par le bot. Optimiste
// (retire immediatement la ligne de la liste locale) plutot que d'attendre
// `revalidatePath` : la reprise du graphe LangGraph cote pm-engine peut
// prendre plusieurs secondes (`MergeAndClose`/`IndexKnowledge` reels).
export function PmReviews({ initialReviews }: { initialReviews: PendingReview[] }) {
  const [reviews, setReviews] = useState(initialReviews);
  const [errors, setErrors] = useState<Record<string, string>>({});
  // Quelle revue est en cours de traitement, et non « une revue quelconque
  // l'est » : `isPending` seul desactivait TOUS les boutons de la liste des
  // qu'on en cliquait un, ce qui bloque l'utilisateur devant dix revues alors
  // qu'une seule est concernee.
  const [deciding, setDeciding] = useState<string | null>(null);
  const [, startTransition] = useTransition();

  function decide(threadId: string, decision: "approved" | "rejected") {
    setErrors((prev) => ({ ...prev, [threadId]: "" }));
    setDeciding(threadId);
    startTransition(async () => {
      try {
        await decideReviewAction(threadId, decision);
        setReviews((prev) => prev.filter((r) => r.threadId !== threadId));
      } catch (err) {
        setErrors((prev) => ({
          ...prev,
          [threadId]: err instanceof Error ? err.message : "erreur inattendue",
        }));
      } finally {
        setDeciding(null);
      }
    });
  }

  if (reviews.length === 0) {
    return (
      <div className="rounded-xl border border-dashed border-border p-8 text-center text-muted text-sm">
        Aucune revue en attente.
      </div>
    );
  }

  return (
    <ul className="flex flex-col gap-3">
      {reviews.map((review) => (
        <li
          key={review.threadId}
          className="rounded-xl border border-border bg-surface p-4 shadow-sm flex flex-col gap-2"
        >
          <div className="flex items-center justify-between gap-3 flex-wrap">
            <div className="min-w-0">
              <span className="font-medium">{review.repo}</span>
              <span className="text-muted"> #{review.issueNumber}</span>
              {review.createdAt && (
                <span className="ml-2 text-xs text-muted">
                  {formatDate(review.createdAt)}
                </span>
              )}
            </div>
            <div className="flex items-center gap-4 text-sm">
              {/* Voir ce que le PM a fait AVANT d'approuver : c'est le
                  premier reflexe attendu, et sans ce lien il fallait
                  retrouver le workflow a la main. */}
              <Link
                href={`/pipeline/${review.threadId.split("/").map(encodeURIComponent).join("/")}`}
                className="text-muted hover:text-foreground transition-colors"
              >
                Voir le pipeline
              </Link>
              {review.prUrl && (
                <a
                  href={review.prUrl}
                  target="_blank"
                  rel="noreferrer"
                  className="text-accent hover:underline"
                >
                  Voir la PR
                </a>
              )}
            </div>
          </div>
          {errors[review.threadId] && (
            <p className="text-sm text-red-600 dark:text-red-400">{errors[review.threadId]}</p>
          )}
          <div className="flex gap-2 justify-end">
            <button
              onClick={() => decide(review.threadId, "rejected")}
              disabled={deciding === review.threadId}
              className="text-sm rounded-full border border-red-500/30 text-red-600 dark:text-red-400 px-4 py-2 hover:bg-red-500/10 transition-colors disabled:opacity-50"
            >
              Rejeter
            </button>
            <button
              onClick={() => decide(review.threadId, "approved")}
              disabled={deciding === review.threadId}
              className="text-sm rounded-full bg-accent text-accent-foreground px-4 py-2 hover:bg-accent-hover transition-colors disabled:opacity-50"
            >
              {deciding === review.threadId ? "Fusion…" : "Approuver et fusionner"}
            </button>
          </div>
        </li>
      ))}
    </ul>
  );
}
