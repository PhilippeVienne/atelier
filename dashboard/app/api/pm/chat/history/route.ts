import { NextResponse } from "next/server";
import { fetchChatHistory, PmEngineError } from "@/lib/pm-engine";

// Route dediee (tache 5.5.1) : meme convention que `app/api/pm/chat/route.ts`
// (token httpOnly ajoute cote serveur uniquement).
export async function GET(request: Request) {
  const repo = new URL(request.url).searchParams.get("repo") ?? "";
  try {
    const history = await fetchChatHistory(repo);
    return NextResponse.json(history);
  } catch (err) {
    if (err instanceof PmEngineError) {
      return NextResponse.json({ message: err.message }, { status: err.status });
    }
    throw err;
  }
}
