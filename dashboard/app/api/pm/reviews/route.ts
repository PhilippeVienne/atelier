import { NextResponse } from "next/server";
import { listPendingReviews, PmEngineError } from "@/lib/pm-engine";

// Tache 5.5.2 : liste des revues HITL en attente, consommee par
// components/pm-reviews.tsx.
export async function GET() {
  try {
    const reviews = await listPendingReviews();
    return NextResponse.json(reviews);
  } catch (err) {
    if (err instanceof PmEngineError) {
      return NextResponse.json({ message: err.message }, { status: err.status });
    }
    throw err;
  }
}
