import Link from "next/link";
import { TopNav } from "@/app/components/top-nav";
import { listPendingReviews, PmEngineError, type PendingReview } from "@/lib/pm-engine";
import { PmChat } from "./pm-chat";
import { PmReviews } from "./pm-reviews";

// Jalon M5, tache 5.5.1/5.5.2 : "Ask Project Manager" + approbation HITL.
// Le plan (`docs/specs/PLAN-ACTION-GLOBAL.md`) situe cette page sous
// `projects/[id]/pm/` ; ce depot n'a pas de notion de "projet" distincte
// d'un depot Git (le PM Engine scope sa memoire par tenant de deploiement,
// pas par projet, voir `pm_engine.rag`) — page unique `/pm` plutot qu'une
// route parametree par un `id` qui n'existe nulle part ailleurs dans ce
// Dashboard.
export default async function PmPage() {
  let reviews: PendingReview[];
  let reviewsError: string | null = null;
  try {
    reviews = await listPendingReviews();
  } catch (err) {
    reviews = [];
    reviewsError =
      err instanceof PmEngineError
        ? err.message
        : "PM Engine injoignable";
  }

  return (
    <>
      <TopNav />
      <main className="flex-1 max-w-2xl w-full mx-auto p-6 sm:p-8 flex flex-col gap-6">
        <div className="flex flex-col gap-1">
          <Link href="/" className="text-sm text-muted hover:text-accent transition-colors">
            ← Workshops
          </Link>
          <h1 className="text-2xl font-semibold tracking-tight">Project Manager</h1>
        </div>

        <PmChat />

        <div className="flex flex-col gap-3">
          <h2 className="text-sm font-medium text-muted uppercase tracking-wide">
            Revues en attente
          </h2>
          {reviewsError && (
            <p className="text-sm text-red-600 dark:text-red-400">{reviewsError}</p>
          )}
          <PmReviews initialReviews={reviews} />
        </div>
      </main>
    </>
  );
}
