import Link from "next/link";
import { notFound } from "next/navigation";
import { ApiServerError, getWorkshop, type WorkshopPhase } from "@/lib/api-server";
import { remove, resume, suspend } from "@/app/actions";

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

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-xs uppercase tracking-wide text-neutral-500">{label}</span>
      <span className="text-sm font-mono break-all">{value}</span>
    </div>
  );
}

export default async function WorkshopDetailPage({
  params,
}: {
  params: Promise<{ name: string }>;
}) {
  const { name } = await params;

  let workshop;
  try {
    workshop = await getWorkshop(name);
  } catch (err) {
    if (err instanceof ApiServerError && err.status === 404) {
      notFound();
    }
    throw err;
  }

  const status = workshop.status;
  const phase = status?.phase ?? "Pending";
  const canSuspend = phase === "Running";
  const canResume = phase === "Suspended";
  const canConnect = phase === "Running";
  const busy = ["BuildingImage", "Provisioning", "Suspending", "Resuming", "Terminating"].includes(
    phase,
  );

  return (
    <main className="flex-1 max-w-2xl w-full mx-auto p-8 flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <div className="flex flex-col gap-1">
          <Link href="/" className="text-sm text-neutral-500 hover:underline">
            ← Workshops
          </Link>
          <h1 className="text-2xl font-semibold">{name}</h1>
        </div>
        <span className={`inline-block rounded-full px-3 py-1 text-sm font-medium ${PHASE_STYLES[phase]}`}>
          {phase}
        </span>
      </div>

      <div className="grid grid-cols-2 gap-4 rounded border border-neutral-200 p-4">
        <Field label="Depot" value={workshop.spec.devcontainer.repo} />
        <Field label="Revision" value={workshop.spec.devcontainer.revision} />
        <Field label="Pod parent" value={status?.podName ?? "—"} />
        <Field label="Image" value={status?.imageDigest ?? "—"} />
        <Field label="Snapshot" value={status?.snapshotDigest ?? "—"} />
      </div>

      <div className="flex flex-wrap gap-2">
        {canConnect && (
          <a
            href={`/workshops/${encodeURIComponent(name)}/vscode/`}
            target="_blank"
            rel="noopener noreferrer"
            className="rounded-full bg-foreground text-background px-4 py-2 text-sm font-medium hover:opacity-90 transition-opacity"
          >
            Ouvrir VS Code ↗
          </a>
        )}
        {canSuspend && (
          <form action={suspend.bind(null, name)}>
            <button
              className="text-sm rounded border border-neutral-300 px-4 py-2 hover:bg-neutral-100"
              disabled={busy}
            >
              Suspendre
            </button>
          </form>
        )}
        {canResume && (
          <form action={resume.bind(null, name)}>
            <button
              className="text-sm rounded border border-neutral-300 px-4 py-2 hover:bg-neutral-100"
              disabled={busy}
            >
              Reprendre
            </button>
          </form>
        )}
        <form action={remove.bind(null, name)}>
          <button
            className="text-sm rounded border border-red-300 text-red-700 px-4 py-2 hover:bg-red-50"
            disabled={busy}
          >
            Supprimer
          </button>
        </form>
      </div>
    </main>
  );
}
