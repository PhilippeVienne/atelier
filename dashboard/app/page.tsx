import Link from "next/link";
import { listWorkshops, type Workshop, type WorkshopPhase } from "@/lib/api-server";
import { logout, remove, resume, suspend } from "@/app/actions";

const PHASE_STYLES: Record<WorkshopPhase, string> = {
  Pending: "bg-neutral-100 text-neutral-700",
  BuildingImage: "bg-amber-100 text-amber-800",
  Provisioning: "bg-amber-100 text-amber-800",
  Running: "bg-green-100 text-green-800",
  Suspending: "bg-amber-100 text-amber-800",
  Suspended: "bg-neutral-200 text-neutral-700",
  Resuming: "bg-amber-100 text-amber-800",
  Terminating: "bg-red-100 text-red-800",
  Failed: "bg-red-100 text-red-800",
};

function PhaseBadge({ phase }: { phase: WorkshopPhase }) {
  return (
    <span className={`inline-block rounded-full px-2.5 py-0.5 text-xs font-medium ${PHASE_STYLES[phase]}`}>
      {phase}
    </span>
  );
}

function WorkshopRow({ workshop }: { workshop: Workshop }) {
  const name = workshop.metadata.name;
  const phase = workshop.status?.phase ?? "Pending";
  const canSuspend = phase === "Running";
  const canResume = phase === "Suspended";
  const busy = ["BuildingImage", "Provisioning", "Suspending", "Resuming", "Terminating"].includes(
    phase,
  );

  return (
    <tr className="border-b border-neutral-200 last:border-0">
      <td className="py-3 pr-4 font-medium">{name}</td>
      <td className="py-3 pr-4">
        <PhaseBadge phase={phase} />
      </td>
      <td className="py-3 pr-4 text-sm text-neutral-500">{workshop.spec.devcontainer.repo}</td>
      <td className="py-3 pr-4 text-sm text-neutral-500 font-mono">
        {workshop.status?.imageDigest?.slice(0, 16) ?? "—"}
      </td>
      <td className="py-3 flex gap-2 justify-end">
        {canSuspend && (
          <form action={suspend.bind(null, name)}>
            <button className="text-sm rounded border border-neutral-300 px-3 py-1 hover:bg-neutral-100" disabled={busy}>
              Suspendre
            </button>
          </form>
        )}
        {canResume && (
          <form action={resume.bind(null, name)}>
            <button className="text-sm rounded border border-neutral-300 px-3 py-1 hover:bg-neutral-100" disabled={busy}>
              Reprendre
            </button>
          </form>
        )}
        <form action={remove.bind(null, name)}>
          <button className="text-sm rounded border border-red-300 text-red-700 px-3 py-1 hover:bg-red-50" disabled={busy}>
            Supprimer
          </button>
        </form>
      </td>
    </tr>
  );
}

export default async function DashboardPage() {
  const workshops = await listWorkshops();

  return (
    <main className="flex-1 max-w-4xl w-full mx-auto p-8 flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">Workshops</h1>
        <div className="flex gap-3">
          <Link
            href="/workshops/new"
            className="rounded-full bg-foreground text-background px-4 py-2 text-sm font-medium hover:opacity-90 transition-opacity"
          >
            Nouveau Workshop
          </Link>
          <form action={logout}>
            <button className="text-sm text-neutral-500 hover:text-neutral-900 px-2">
              Se deconnecter
            </button>
          </form>
        </div>
      </div>

      {workshops.length === 0 ? (
        <p className="text-neutral-500">Aucun Workshop pour le moment.</p>
      ) : (
        <table className="w-full text-left">
          <thead>
            <tr className="border-b border-neutral-300 text-sm text-neutral-500">
              <th className="py-2 pr-4 font-medium">Nom</th>
              <th className="py-2 pr-4 font-medium">Phase</th>
              <th className="py-2 pr-4 font-medium">Depot</th>
              <th className="py-2 pr-4 font-medium">Image</th>
              <th className="py-2" />
            </tr>
          </thead>
          <tbody>
            {workshops.map((w) => (
              <WorkshopRow key={w.metadata.name} workshop={w} />
            ))}
          </tbody>
        </table>
      )}
    </main>
  );
}
