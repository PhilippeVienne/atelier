import { NextResponse } from "next/server";
import { ApiServerError, listWorkshopEvents } from "@/lib/api-server";

// Route dediee (plutot que de laisser le composant client appeler
// directement atelier-api-server) parce que le token de session est
// httpOnly : seul du code serveur peut le lire (cf. lib/session.ts). Le
// polling client (voir workshops/[name]/events-log.tsx) tape ici.
export async function GET(
  _request: Request,
  { params }: { params: Promise<{ name: string }> },
) {
  const { name } = await params;
  try {
    const events = await listWorkshopEvents(name);
    return NextResponse.json(events);
  } catch (err) {
    if (err instanceof ApiServerError) {
      return NextResponse.json({ message: err.message }, { status: err.status });
    }
    throw err;
  }
}
