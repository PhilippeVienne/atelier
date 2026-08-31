import { NextResponse } from "next/server";
import { getWorkflow, PmEngineError } from "@/lib/pm-engine";

// Etat d'un workflow PM pour la vue « mission control »
// (app/pipeline/[...threadId]). Sonde par le client toutes les quelques
// secondes pendant qu'un pipeline tourne.
//
// Route attrape-tout (`[...threadId]`) : un thread_id vaut `owner/repo#42`
// et contient donc une barre oblique, qui arrive ici en segments separes.
//
// Les phases des microVM viennent du pm-engine, pas d'un appel direct a
// l'api-server : les Workshops du graphe appartiennent a l'identite de
// service `atelier-pm-bot`, et l'api-server en refuse la lecture a tout
// autre sujet. Interroges au nom de l'utilisateur connecte, ils
// repondaient « inconnu » en permanence.
export async function GET(
  _request: Request,
  { params }: { params: Promise<{ threadId: string[] }> },
) {
  const { threadId } = await params;
  const id = threadId.map(decodeURIComponent).join("/");
  try {
    return NextResponse.json(await getWorkflow(id));
  } catch (err) {
    if (err instanceof PmEngineError) {
      return NextResponse.json({ message: err.message }, { status: err.status });
    }
    throw err;
  }
}
