import Link from "next/link";
import { TopNav } from "@/app/components/top-nav";
import { listPendingReviews, PmEngineError, type PendingReview } from "@/lib/pm-engine";
import { PmReviews } from "./pm-reviews";

// Jalon M5, tache 5.5.2 : approbation HITL. Le chat (tache 5.5.1) a
// deménagé sur "/" (page d'accueil, voir app/page.tsx) - cette page ne
// porte plus que la file de revues en attente.
export default async function PmReviewsPage() {
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
            ← Chat
          </Link>
          <h1 className="text-2xl font-semibold tracking-tight">Revues en attente</h1>
        </div>

        {reviewsError && (
          <p className="text-sm text-red-600 dark:text-red-400">{reviewsError}</p>
        )}
        <PmReviews initialReviews={reviews} />
      </main>
    </>
  );
}
