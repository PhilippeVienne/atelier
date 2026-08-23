import Link from "next/link";
import { listWorkshops, type Workshop } from "@/lib/api-server";
import { logout, remove, resume, suspend } from "@/app/actions";
import { PhaseBadge } from "@/app/components/phase-badge";
import { TopNav } from "@/app/components/top-nav";

function WorkshopCard({ workshop }: { workshop: Workshop }) {
  const name = workshop.metadata.name;
  const phase = workshop.status?.phase ?? "Pending";
  const canSuspend = phase === "Running";
  const canResume = phase === "Suspended";
  const busy = ["BuildingImage", "Provisioning", "Suspending", "Resuming", "Terminating"].includes(
    phase,
  );

  return (
    <li className="rounded-xl border border-border bg-surface p-5 shadow-sm hover:shadow-md transition-shadow flex flex-col gap-3">
      <div className="flex items-start justify-between gap-3">
        <Link
          href={`/workshops/${encodeURIComponent(name)}`}
          className="font-medium hover:text-accent transition-colors truncate"
        >
          {name}
        </Link>
        <PhaseBadge phase={phase} />
      </div>
      <p className="text-sm text-muted truncate">{workshop.spec.devcontainer.repo}</p>
      <p className="text-xs text-muted font-mono truncate">
        {workshop.status?.imageDigest?.slice(0, 24) ?? "image non construite"}
      </p>
      <div className="flex gap-2 justify-end pt-1">
        {canSuspend && (
          <form action={suspend.bind(null, name)}>
            <button
              className="text-sm rounded-full border border-border px-3 py-1 hover:bg-surface-hover transition-colors"
              disabled={busy}
            >
              Suspendre
            </button>
          </form>
        )}
        {canResume && (
          <form action={resume.bind(null, name)}>
            <button
              className="text-sm rounded-full border border-border px-3 py-1 hover:bg-surface-hover transition-colors"
              disabled={busy}
            >
              Reprendre
            </button>
          </form>
        )}
        <form action={remove.bind(null, name)}>
          <button
            className="text-sm rounded-full border border-red-500/30 text-red-600 dark:text-red-400 px-3 py-1 hover:bg-red-500/10 transition-colors"
            disabled={busy}
          >
            Supprimer
          </button>
        </form>
      </div>
    </li>
  );
}

export default async function DashboardPage() {
  const workshops = await listWorkshops();

  return (
    <>
      <TopNav>
        <form action={logout}>
          <button className="text-sm text-muted hover:text-foreground transition-colors px-2">
            Se deconnecter
          </button>
        </form>
      </TopNav>
      <main className="flex-1 max-w-5xl w-full mx-auto p-6 sm:p-8 flex flex-col gap-6">
        <div className="flex items-center justify-between flex-wrap gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">Workshops</h1>
          <div className="flex gap-3">
            <Link
              href="/workshops/new?preset=ministack"
              className="rounded-full border border-border px-4 py-2 text-sm font-medium hover:bg-surface-hover transition-colors"
            >
              Demo ministack
            </Link>
            <Link
              href="/workshops/new"
              className="rounded-full bg-accent text-accent-foreground px-4 py-2 text-sm font-medium hover:bg-accent-hover transition-colors"
            >
              Nouveau Workshop
            </Link>
          </div>
        </div>

        {workshops.length === 0 ? (
          <div className="rounded-xl border border-dashed border-border p-12 text-center text-muted">
            Aucun Workshop pour le moment.
          </div>
        ) : (
          <ul className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            {workshops.map((w) => (
              <WorkshopCard key={w.metadata.name} workshop={w} />
            ))}
          </ul>
        )}
      </main>
    </>
  );
}
