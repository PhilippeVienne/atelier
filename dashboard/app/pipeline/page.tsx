import Link from "next/link";
import { TopNav } from "@/app/components/top-nav";
import { logout } from "@/app/actions";
import { listWorkflows, PmEngineError, type WorkflowSummary } from "@/lib/pm-engine";
import { forgejoMirrorEnabled, listProjects } from "@/lib/forgejo";
import { Launcher } from "./launcher";

// Point d'entree de la vue « mission control » : lancer un ticket, ou
// reprendre le suivi d'un workflow deja demarre.
export default async function PipelineIndex({
  searchParams,
}: {
  searchParams: Promise<{ repo?: string }>;
}) {
  // `?repo=` preselectionne le projet : le lien « Lancer un ticket » de la
  // page Projets arrive ici avec un projet deja choisi, sans quoi il faudrait
  // le re-selectionner alors qu'on vient justement de le designer.
  const { repo: preselected } = await searchParams;
  let workflows: WorkflowSummary[] = [];
  try {
    workflows = await listWorkflows();
  } catch (err) {
    // Le lanceur reste utilisable meme si l'historique est indisponible.
    if (!(err instanceof PmEngineError)) throw err;
  }

  let projects: string[] = [];
  if (forgejoMirrorEnabled()) {
    try {
      projects = (await listProjects()).map((p) => p.fullName);
    } catch {
      projects = [];
    }
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

      <div className="mx-auto w-full max-w-5xl px-4 py-6 flex flex-col gap-6">
        <Launcher projects={projects} preselected={preselected} />

        <section className="flex flex-col gap-3">
          <h2 className="text-sm font-semibold text-muted">Workflows récents</h2>
          {workflows.length === 0 ? (
            <p className="rounded-xl border border-dashed border-border p-6 text-center text-sm text-muted">
              Aucun workflow pour l&apos;instant. Lancez un ticket ci-dessus.
            </p>
          ) : (
            <ul className="flex flex-col gap-2">
              {workflows.map((w) => {
                const path = w.threadId.split("/").map(encodeURIComponent).join("/");
                return (
                  <li key={w.threadId}>
                    <Link
                      href={`/pipeline/${path}`}
                      className="flex items-center gap-3 rounded-lg border border-border bg-surface/70 px-4 py-3 text-sm transition-colors hover:bg-surface-hover"
                    >
                      <span className="shrink-0 rounded-full bg-accent/10 px-2 py-0.5 text-xs font-medium text-accent">
                        #{w.issueNumber}
                      </span>
                      <span className="min-w-0 flex-1 truncate">
                        {w.issueTitle ?? w.repo}
                      </span>
                      {/* Resultat des tests : l'information la plus utile pour
                          retrouver un run d'un coup d'oeil. `null` = pas encore
                          execute, a distinguer d'un echec. */}
                      {w.testPassed === true && (
                        <span className="shrink-0 text-xs text-emerald-600 dark:text-emerald-400">
                          tests verts
                        </span>
                      )}
                      {w.testPassed === false && (
                        <span className="shrink-0 text-xs text-red-600 dark:text-red-400">
                          tests en échec
                        </span>
                      )}
                      <span className="shrink-0 font-mono text-xs text-muted">
                        {w.phaseIndex >= 0 ? `${w.phaseIndex + 1}/11` : "—"}
                      </span>
                    </Link>
                  </li>
                );
              })}
            </ul>
          )}
        </section>
      </div>
    </div>
  );
}
