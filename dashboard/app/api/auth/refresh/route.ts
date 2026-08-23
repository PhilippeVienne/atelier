import { NextResponse } from "next/server";
import { getAccessToken } from "@/lib/session";

// Appelee periodiquement par `SessionKeepAlive` (client) pour que la
// session reste valide tant que l'onglet est ouvert — `getAccessToken()`
// rafraichit deja silencieusement via le refresh token si necessaire (voir
// `lib/session.ts`), cette route ne fait qu'exposer ce mecanisme au
// navigateur et rapporter si la session est (encore) valide.
export async function POST() {
  const token = await getAccessToken();
  if (!token) {
    return NextResponse.json({ ok: false }, { status: 401 });
  }
  return NextResponse.json({ ok: true });
}
