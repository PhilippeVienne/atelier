import { NextResponse } from "next/server";
import { decideReview, PmEngineError } from "@/lib/pm-engine";

// Tache 5.5.2 : approuve/rejette la PR ouverte par le bot pour ce
// `thread_id`, reprend le graphe LangGraph cote pm-engine.
export async function POST(
  request: Request,
  { params }: { params: Promise<{ threadId: string }> },
) {
  const { threadId } = await params;
  const body = (await request.json().catch(() => null)) as { decision?: string } | null;
  if (body?.decision !== "approved" && body?.decision !== "rejected") {
    return NextResponse.json(
      { message: "decision doit etre 'approved' ou 'rejected'" },
      { status: 400 },
    );
  }
  try {
    const result = await decideReview(threadId, body.decision);
    return NextResponse.json(result);
  } catch (err) {
    if (err instanceof PmEngineError) {
      return NextResponse.json({ message: err.message }, { status: err.status });
    }
    throw err;
  }
}
