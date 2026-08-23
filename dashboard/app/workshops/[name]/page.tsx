import Link from "next/link";
import { notFound } from "next/navigation";
import { ApiServerError, getWorkshop, listWorkshopEvents } from "@/lib/api-server";
import { remove, resume, suspend } from "@/app/actions";
import { PhaseBadge } from "@/app/components/phase-badge";
import { TopNav } from "@/app/components/top-nav";
import { EventsLog } from "./events-log";
import { LiveRefresh } from "./live-refresh";

const BUSY_PHASES = ["BuildingImage", "Provisioning", "Suspending", "Resuming", "Terminating"];

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-xs uppercase tracking-wide text-muted">{label}</span>
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

  const events = await listWorkshopEvents(name).catch(() => []);

  const status = workshop.status;
  const phase = status?.phase ?? "Pending";
  const canSuspend = phase === "Running";
  const canResume = phase === "Suspended";
  const canConnect = phase === "Running";
  const busy = BUSY_PHASES.includes(phase);

  return (
    <>
      <LiveRefresh active={busy} />
      <TopNav />
      <main className="flex-1 max-w-2xl w-full mx-auto p-6 sm:p-8 flex flex-col gap-6">
        <div className="flex items-center justify-between gap-4">
          <div className="flex flex-col gap-1">
            <Link href="/" className="text-sm text-muted hover:text-accent transition-colors">
              ← Workshops
            </Link>
            <h1 className="text-2xl font-semibold tracking-tight">{name}</h1>
          </div>
          <PhaseBadge phase={phase} size="md" />
        </div>

        <div className="grid grid-cols-2 gap-5 rounded-xl border border-border bg-surface p-5 shadow-sm">
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
              className="rounded-full bg-accent text-accent-foreground px-4 py-2 text-sm font-medium hover:bg-accent-hover transition-colors"
            >
              Ouvrir VS Code ↗
            </a>
          )}
          {canSuspend && (
            <form action={suspend.bind(null, name)}>
              <button
                className="text-sm rounded-full border border-border px-4 py-2 hover:bg-surface-hover transition-colors"
                disabled={busy}
              >
                Suspendre
              </button>
            </form>
          )}
          {canResume && (
            <form action={resume.bind(null, name)}>
              <button
                className="text-sm rounded-full border border-border px-4 py-2 hover:bg-surface-hover transition-colors"
                disabled={busy}
              >
                Reprendre
              </button>
            </form>
          )}
          <form action={remove.bind(null, name)}>
            <button
              className="text-sm rounded-full border border-red-500/30 text-red-600 dark:text-red-400 px-4 py-2 hover:bg-red-500/10 transition-colors"
              disabled={busy}
            >
              Supprimer
            </button>
          </form>
        </div>

        <div className="flex flex-col gap-3">
          <h2 className="text-sm font-medium text-muted uppercase tracking-wide">
            Journal de creation / progression
          </h2>
          <EventsLog name={name} initialEvents={events} live={busy} />
        </div>
      </main>
    </>
  );
}
