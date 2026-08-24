"use client";

import { useState, useTransition } from "react";
import { decideReviewAction } from "@/app/actions";
import type { PendingReview } from "@/lib/pm-engine";

// Tache 5.5.2 : approuve/rejette une PR ouverte par le bot. Optimiste
// (retire immediatement la ligne de la liste locale) plutot que d'attendre
// `revalidatePath` : la reprise du graphe LangGraph cote pm-engine peut
// prendre plusieurs secondes (`MergeAndClose`/`IndexKnowledge` reels).
export function PmReviews({ initialReviews }: { initialReviews: PendingReview[] }) {
  const [reviews, setReviews] = useState(initialReviews);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [isPending, startTransition] = useTransition();

  function decide(threadId: string, decision: "approved" | "rejected") {
    setErrors((prev) => ({ ...prev, [threadId]: "" }));
    startTransition(async () => {
      try {
        await decideReviewAction(threadId, decision);
        setReviews((prev) => prev.filter((r) => r.threadId !== threadId));
      } catch (err) {
        setErrors((prev) => ({
          ...prev,
          [threadId]: err instanceof Error ? err.message : "erreur inattendue",
        }));
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
            <div>
              <span className="font-medium">{review.repo}</span>
              <span className="text-muted"> #{review.issueNumber}</span>
            </div>
            {review.prUrl && (
              <a
                href={review.prUrl}
                target="_blank"
                rel="noreferrer"
                className="text-sm text-accent hover:underline"
              >
                Voir la PR
              </a>
            )}
          </div>
          {errors[review.threadId] && (
            <p className="text-sm text-red-600 dark:text-red-400">{errors[review.threadId]}</p>
          )}
          <div className="flex gap-2 justify-end">
            <button
              onClick={() => decide(review.threadId, "rejected")}
              disabled={isPending}
              className="text-sm rounded-full border border-red-500/30 text-red-600 dark:text-red-400 px-4 py-2 hover:bg-red-500/10 transition-colors disabled:opacity-50"
            >
              Rejeter
            </button>
            <button
              onClick={() => decide(review.threadId, "approved")}
              disabled={isPending}
              className="text-sm rounded-full bg-accent text-accent-foreground px-4 py-2 hover:bg-accent-hover transition-colors disabled:opacity-50"
            >
              Approuver et fusionner
            </button>
          </div>
        </li>
      ))}
    </ul>
  );
}
