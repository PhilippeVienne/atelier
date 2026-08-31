import { NextResponse } from "next/server";
import { launchWorkflow, listWorkflows, PmEngineError } from "@/lib/pm-engine";

// Lancement et listing des workflows PM (vue « mission control »).
export async function GET() {
  try {
    return NextResponse.json(await listWorkflows());
  } catch (err) {
    if (err instanceof PmEngineError) {
      return NextResponse.json({ message: err.message }, { status: err.status });
    }
    throw err;
  }
}

export async function POST(request: Request) {
  const body = (await request.json()) as { repo?: string; issueNumber?: number };
  if (!body.repo || !body.issueNumber) {
    return NextResponse.json(
      { message: "repo et issueNumber sont requis" },
      { status: 400 },
    );
  }
  try {
    // `devcontainerRepo` volontairement absent : c'est le pm-engine qui le
    // deduit de son gabarit de deploiement, pour qu'aucun identifiant de
    // clone ne transite par le navigateur.
    const { threadId } = await launchWorkflow(body.repo, body.issueNumber);
    return NextResponse.json({ threadId });
  } catch (err) {
    if (err instanceof PmEngineError) {
      return NextResponse.json({ message: err.message }, { status: err.status });
    }
    throw err;
  }
}
