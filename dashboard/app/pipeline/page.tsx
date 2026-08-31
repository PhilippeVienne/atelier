import Link from "next/link";
import { TopNav } from "@/app/components/top-nav";
import { logout } from "@/app/actions";
import { listWorkflows, PmEngineError } from "@/lib/pm-engine";
import { forgejoMirrorEnabled, listProjects } from "@/lib/forgejo";
import { Launcher } from "./launcher";

// Point d'entree de la vue « mission control » : lancer un ticket, ou
// reprendre le suivi d'un workflow deja demarre.
export default async function PipelineIndex() {
  let workflows: string[] = [];
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

  // Les threads de workflow valent `owner/depot#42` ; les autres (chat,
  // tests) n'ont pas ce format et n'ont rien a faire dans cette liste.
  const tickets = workflows.filter((t) => t.includes("#"));

  return (
    <div className="min-h-dvh flex flex-col">
      <TopNav className="border-b border-border bg-surface/80 backdrop-blur supports-[backdrop-filter]:bg-surface/60">
        <Link
          href="/pm"
          className="text-sm text-muted hover:text-foreground transition-colors px-2"
        >
          Revues
        </Link>
        <form action={logout}>
          <button className="text-sm text-muted hover:text-foreground transition-colors px-2">
            Se deconnecter
          </button>
        </form>
      </TopNav>

      <div className="mx-auto w-full max-w-5xl px-4 py-6 flex flex-col gap-6">
        <Launcher projects={projects} />

        <section className="flex flex-col gap-3">
          <h2 className="text-sm font-semibold text-muted">Workflows recents</h2>
          {tickets.length === 0 ? (
            <p className="rounded-xl border border-dashed border-border p-6 text-center text-sm text-muted">
              Aucun workflow pour l&apos;instant. Lancez un ticket ci-dessus.
            </p>
          ) : (
            <ul className="flex flex-col gap-2">
              {tickets.map((threadId) => {
                const path = threadId.split("/").map(encodeURIComponent).join("/");
                const [repo, issue] = threadId.split("#");
                return (
                  <li key={threadId}>
                    <Link
                      href={`/pipeline/${path}`}
                      className="flex items-center justify-between rounded-lg border border-border bg-surface/70 px-4 py-3 text-sm transition-colors hover:bg-surface-hover"
                    >
                      <span className="font-mono truncate">{repo}</span>
                      <span className="ml-3 shrink-0 rounded-full bg-accent/10 px-2 py-0.5 text-xs font-medium text-accent">
                        #{issue}
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
