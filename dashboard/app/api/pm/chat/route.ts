import { NextResponse } from "next/server";
import { type ChatMessage, PmEngineError, proxyChat } from "@/lib/pm-engine";

// Route dediee (tache 5.5.1) : le token de session est httpOnly, seul du
// code serveur peut le lire (cf. lib/session.ts) — le composant client de
// chat (components/pm-chat.tsx) tape ici, jamais directement sur
// pm-engine.
export async function POST(request: Request) {
  const body = (await request.json().catch(() => null)) as
    | { repo?: string; query?: string; history?: ChatMessage[] }
    | null;
  // `repo` optionnel : une question generale (fonctionnement, import d'un
  // projet) n'a pas a cibler un depot — voir `PmChat`.
  if (!body?.query) {
    return NextResponse.json({ message: "query requis" }, { status: 400 });
  }
  try {
    return await proxyChat(body.repo ?? "", body.query, body.history ?? []);
  } catch (err) {
    if (err instanceof PmEngineError) {
      return NextResponse.json({ message: err.message }, { status: err.status });
    }
    throw err;
  }
}
