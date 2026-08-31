import Link from "next/link";
import { TopNav } from "@/app/components/top-nav";
import { forgejoMirrorEnabled, listProjects } from "@/lib/forgejo";

export default async function ProjectsPage() {
  const enabled = forgejoMirrorEnabled();
  const projects = enabled ? await listProjects() : [];

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
          Un projet est un dépôt de la forge interne sur lequel le PM peut agir : lire des
          tickets, ouvrir des PR, provisionner des Workshops. Il peut être <strong>natif</strong>{" "}
          (créé directement dans la forge) ou <strong>miroir</strong> d&apos;un dépôt
          GitHub/GitLab, resynchronise automatiquement — dans ce cas le PM et les Workshops
          travaillent sur le miroir, jamais sur le dépôt source.
        </p>

        {!enabled ? (
          <div className="rounded-xl border border-dashed border-border p-12 text-center text-muted">
            Miroir Forgejo non configure sur ce dashboard (variable
            <code className="mx-1 font-mono text-xs">ATELIER_FORGEJO_ADMIN_TOKEN</code>
            absente).
          </div>
        ) : projects.length === 0 ? (
          <div className="rounded-xl border border-dashed border-border p-12 text-center text-muted">
            Aucun projet pour le moment.
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
                  <span className="flex gap-1.5 shrink-0">
                    <span className="text-xs rounded-full border border-border px-2 py-0.5 text-muted">
                      {project.isMirror ? "miroir" : "natif"}
                    </span>
                    {project.private && (
                      <span className="text-xs rounded-full border border-border px-2 py-0.5 text-muted">
                        prive
                      </span>
                    )}
                  </span>
                </div>
                {project.originalUrl && (
                  <p className="text-sm text-muted truncate">Source : {project.originalUrl}</p>
                )}
                <p className="text-xs text-muted font-mono truncate">{project.cloneUrl}</p>
                {/* Deux actions, dans l'ordre de ce qu'on veut faire d'un
                    projet : confier un ticket au PM (le coeur d'Atelier),
                    ou ouvrir un environnement pour travailler soi-meme. */}
                <div className="flex flex-wrap justify-end gap-2 pt-1">
                  <Link
                    href={`/pipeline?repo=${encodeURIComponent(project.fullName)}`}
                    className="text-sm rounded-full bg-accent text-accent-foreground px-3 py-1 hover:bg-accent-hover transition-colors"
                  >
                    Lancer un ticket
                  </Link>
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
