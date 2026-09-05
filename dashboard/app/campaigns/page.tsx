import Link from "next/link";
import { listWorkshops, type Workshop } from "@/lib/api-server";
import { logout } from "@/app/actions";
import { PhaseBadge } from "@/app/components/phase-badge";
import { TopNav } from "@/app/components/top-nav";

/** Regroupe les Workshops par `campaignId` (tache 12.6, spec docs/specs/16-
 *  escouades-multi-agents-swarms-mesh.md §3.2) — aucun nouvel endpoint
 *  api-server necessaire, `listWorkshops()` porte deja tout ce qu'il faut. */
function groupByCampaign(workshops: Workshop[]): Map<string, Workshop[]> {
  const campaigns = new Map<string, Workshop[]>();
  for (const workshop of workshops) {
    const campaignId = workshop.spec.campaignId;
    if (!campaignId) continue;
    const existing = campaigns.get(campaignId);
    if (existing) {
      existing.push(workshop);
    } else {
      campaigns.set(campaignId, [workshop]);
    }
  }
  return campaigns;
}

function WorkshopRow({ workshop, allInCampaign }: { workshop: Workshop; allInCampaign: Workshop[] }) {
  const name = workshop.metadata.name;
  const phase = workshop.status?.phase ?? "Pending";
  const exported = workshop.spec.exportedServices;
  const targets = workshop.spec.allowedInternalTargets;

  return (
    <li className="rounded-lg border border-border bg-surface p-4 flex flex-col gap-2">
      <div className="flex items-center justify-between gap-3 flex-wrap">
        <Link
          href={`/workshops/${encodeURIComponent(name)}`}
          className="font-medium hover:text-accent transition-colors truncate"
        >
          {name}
        </Link>
        <PhaseBadge phase={phase} />
      </div>
      {exported.length > 0 && (
        <p className="text-xs text-muted">
          Expose :{" "}
          {exported.map((s) => (
            <code
              key={s.name}
              className="rounded bg-surface-hover px-1.5 py-0.5 mr-1 font-mono"
            >
              {s.name}:{s.port}
            </code>
          ))}
        </p>
      )}
      {targets.length > 0 && (
        <p className="text-xs text-muted">
          Joint :{" "}
          {targets.map((t) => {
            // `<service>.<workshop-cible>.atelier.internal:<port>` — retrouve
            // le Workshop cible dans la MEME campagne pour un lien direct
            // quand c'est possible (best-effort : une cible hors campagne,
            // improbable mais pas interdite par le CRD, reste affichee en
            // texte simple).
            const targetWorkshopName = t.split(".").slice(1, -2).join(".");
            const targetWorkshop = allInCampaign.find(
              (w) => w.metadata.name === targetWorkshopName,
            );
            return (
              <code key={t} className="rounded bg-surface-hover px-1.5 py-0.5 mr-1 font-mono">
                {targetWorkshop ? (
                  <Link
                    href={`/workshops/${encodeURIComponent(targetWorkshop.metadata.name)}`}
                    className="hover:text-accent transition-colors"
                  >
                    {t}
                  </Link>
                ) : (
                  t
                )}
              </code>
            );
          })}
        </p>
      )}
    </li>
  );
}

function CampaignSection({ campaignId, workshops }: { campaignId: string; workshops: Workshop[] }) {
  const runningCount = workshops.filter((w) => w.status?.phase === "Running").length;
  return (
    <section className="rounded-xl border border-border bg-surface p-5 shadow-sm flex flex-col gap-3">
      <div className="flex items-center justify-between gap-3 flex-wrap">
        <h2 className="font-semibold tracking-tight truncate" title={campaignId}>
          {campaignId}
        </h2>
        <span className="text-xs text-muted whitespace-nowrap">
          {runningCount}/{workshops.length} en cours d&apos;exécution
        </span>
      </div>
      <ul className="flex flex-col gap-2">
        {workshops.map((w) => (
          <WorkshopRow key={w.metadata.name} workshop={w} allInCampaign={workshops} />
        ))}
      </ul>
    </section>
  );
}

export default async function CampaignsPage() {
  const workshops = await listWorkshops();
  const campaigns = groupByCampaign(workshops);

  return (
    <>
      <TopNav>
        <form action={logout}>
          <button className="whitespace-nowrap text-sm text-muted hover:text-foreground transition-colors px-2">
            Se déconnecter
          </button>
        </form>
      </TopNav>
      <main className="flex-1 max-w-5xl w-full mx-auto p-6 sm:p-8 flex flex-col gap-6">
        <div className="flex items-center justify-between flex-wrap gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">Campagnes</h1>
        </div>

        {campaigns.size === 0 ? (
          <div className="rounded-xl border border-dashed border-border p-12 text-center text-muted">
            Aucune campagne multi-Workshops en cours — un{" "}
            <code className="font-mono">campaignId</code> commun relie plusieurs Workshops
            spécialisés (backend/frontend/QA) qui se joignent en HTTP (spec{" "}
            <code className="font-mono">
              docs/specs/16-escouades-multi-agents-swarms-mesh.md
            </code>
            ).
          </div>
        ) : (
          <div className="flex flex-col gap-4">
            {Array.from(campaigns.entries()).map(([campaignId, campaignWorkshops]) => (
              <CampaignSection
                key={campaignId}
                campaignId={campaignId}
                workshops={campaignWorkshops}
              />
            ))}
          </div>
        )}
      </main>
    </>
  );
}
