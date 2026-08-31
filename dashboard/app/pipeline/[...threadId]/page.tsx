import Link from "next/link";
import { notFound } from "next/navigation";
import { TopNav } from "@/app/components/top-nav";
import { logout } from "@/app/actions";
import { getWorkflow, PmEngineError } from "@/lib/pm-engine";
import { PipelineView } from "./pipeline-view";

// Vue « mission control » d'un workflow PM (Jalon M5) : le pipeline complet
// — decoupage, microVM en parallele, integration, tests, PR — rendu visible
// pendant qu'il tourne.
//
// Le premier rendu est fait cote serveur, pour que la page arrive deja
// remplie : en demo, une page qui s'affiche vide puis se peuple apres un
// aller-retour donne l'impression que rien ne marche.
export default async function PipelinePage({
  params,
}: {
  params: Promise<{ threadId: string[] }>;
}) {
  const { threadId } = await params;
  const id = threadId.map(decodeURIComponent).join("/");

  let workflow;
  try {
    workflow = await getWorkflow(id);
  } catch (err) {
    if (err instanceof PmEngineError && err.status === 404) notFound();
    throw err;
  }

  return (
    <div className="min-h-dvh flex flex-col">
      <TopNav className="border-b border-border bg-surface/80 backdrop-blur supports-[backdrop-filter]:bg-surface/60">
        <Link
          href="/pm"
          className="whitespace-nowrap text-sm text-muted hover:text-foreground transition-colors px-2"
        >
          Revues
        </Link>
        <form action={logout}>
          <button className="whitespace-nowrap text-sm text-muted hover:text-foreground transition-colors px-2">
            Se déconnecter
          </button>
        </form>
      </TopNav>
      <PipelineView threadId={id} initial={workflow} />
    </div>
  );
}
