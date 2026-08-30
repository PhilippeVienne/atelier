import Link from "next/link";
import { TopNav } from "@/app/components/top-nav";
import { forgejoMirrorEnabled, listMirrorProjects } from "@/lib/forgejo";

export default async function ProjectsPage() {
  const enabled = forgejoMirrorEnabled();
  const projects = enabled ? await listMirrorProjects() : [];

  return (
    <>
      <TopNav />
      <main className="flex-1 max-w-5xl w-full mx-auto p-6 sm:p-8 flex flex-col gap-6">
        <div className="flex items-center justify-between flex-wrap gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">Projets</h1>
          <Link
            href="/projects/new"
            className="rounded-full bg-accent text-accent-foreground px-4 py-2 text-sm font-medium hover:bg-accent-hover transition-colors"
          >
            Importer un projet
          </Link>
        </div>
        <p className="text-sm text-muted max-w-2xl">
          Un projet mire un depot GitHub/GitLab (prive ou public) vers la forge Forgejo interne
          (resynchronisation automatique) : le PM Engine et les Workshops travaillent ensuite
          uniquement sur ce miroir, jamais directement sur le depot source.
        </p>

        {!enabled ? (
          <div className="rounded-xl border border-dashed border-border p-12 text-center text-muted">
            Miroir Forgejo non configure sur ce dashboard (variable
            <code className="mx-1 font-mono text-xs">ATELIER_FORGEJO_ADMIN_TOKEN</code>
            absente).
          </div>
        ) : projects.length === 0 ? (
          <div className="rounded-xl border border-dashed border-border p-12 text-center text-muted">
            Aucun projet importe pour le moment.
          </div>
        ) : (
          <ul className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            {projects.map((project) => (
              <li
                key={project.fullName}
                className="rounded-xl border border-border bg-surface p-5 shadow-sm flex flex-col gap-3"
              >
                <div className="flex items-start justify-between gap-3">
                  <span className="font-medium truncate">{project.fullName}</span>
                  {project.private && (
                    <span className="text-xs rounded-full border border-border px-2 py-0.5 text-muted shrink-0">
                      prive
                    </span>
                  )}
                </div>
                {project.originalUrl && (
                  <p className="text-sm text-muted truncate">Source : {project.originalUrl}</p>
                )}
                <p className="text-xs text-muted font-mono truncate">{project.cloneUrl}</p>
                <div className="flex justify-end pt-1">
                  <Link
                    href={`/workshops/new?repo=${encodeURIComponent(project.cloneUrl)}`}
                    className="text-sm rounded-full border border-border px-3 py-1 hover:bg-surface-hover transition-colors"
                  >
                    Nouveau Workshop
                  </Link>
                </div>
              </li>
            ))}
          </ul>
        )}
      </main>
    </>
  );
}
