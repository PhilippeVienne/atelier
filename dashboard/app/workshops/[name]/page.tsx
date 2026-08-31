import Link from "next/link";
import { notFound } from "next/navigation";
import {
  ApiServerError,
  getLlmBudget,
  getWorkshop,
  listCredentials,
  listWorkshopEvents,
} from "@/lib/api-server";
import { remove, resume, suspend } from "@/app/actions";
import { PhaseBadge } from "@/app/components/phase-badge";
import { TopNav } from "@/app/components/top-nav";
import { EventsLog } from "./events-log";
import { LiveRefresh } from "./live-refresh";
import { ConnectLink, TerminalFrame } from "./connect";
import { Credentials } from "./credentials";

const BUSY_PHASES = ["BuildingImage", "Provisioning", "Suspending", "Resuming", "Terminating"];

/** Masque les identifiants d'une URL avant affichage.
 *
 * Une URL de clone peut porter un `user:token@` — c'est le cas du raccourci
 * de demonstration du PM. L'afficher tel quel mettait un jeton Forgejo en
 * clair a l'ecran, dans une page qui sert justement a gerer des secrets.
 * Le nom d'utilisateur reste visible : il aide a comprendre l'acces utilise,
 * sans rien donner. */
export function maskUrlCredentials(value: string): string {
  return value.replace(
    /(\w+:\/\/)([^/@\s:]+):([^/@\s]+)@/g,
    (_m, scheme, user) => `${scheme}${user}:••••••@`,
  );
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-xs uppercase tracking-wide text-muted">{label}</span>
      <span className="text-sm font-mono break-all">{value}</span>
    </div>
  );
}

/** Consommation LLM du Workshop.
 *
 * On distingue trois cas, parce qu'ils n'ont pas le meme sens : un plafond
 * configure (on peut montrer la marge restante), aucun plafond (la depense
 * seule), et aucune Virtual Key (la depense n'est pas « zero », elle est
 * inconnue). Afficher « 0,00 $ » dans ce dernier cas laisserait croire a une
 * mesure. */
function LlmBudgetCard({
  budget,
}: {
  budget: { spendUsd: number; maxBudgetUsd: number | null; keyCount: number };
}) {
  // `narrowSymbol` : la locale fr rend USD en « $US », lourd a lire dans un
  // rapport de consommation ou le symbole se repete.
  const money = (v: number) =>
    v.toLocaleString("fr-FR", {
      style: "currency",
      currency: "USD",
      currencyDisplay: "narrowSymbol",
      minimumFractionDigits: 2,
      maximumFractionDigits: 4,
    });
  const ratio =
    budget.maxBudgetUsd && budget.maxBudgetUsd > 0
      ? Math.min(1, budget.spendUsd / budget.maxBudgetUsd)
      : null;
  // Au-dela de 80 % le plafond devient une information urgente : LiteLLM
  // refusera les appels des qu'il sera atteint, et l'agent s'arretera net.
  const tone =
    ratio === null ? "bg-accent" : ratio >= 0.8 ? "bg-red-500" : ratio >= 0.5 ? "bg-amber-500" : "bg-emerald-500";

  return (
    <div className="rounded-xl border border-border bg-surface p-5 shadow-sm flex flex-col gap-3">
      <div className="flex items-baseline justify-between gap-3">
        <span className="text-xs uppercase tracking-wide text-muted">Crédit LLM</span>
        {budget.keyCount === 0 ? (
          <span className="text-sm text-muted">aucune clé — consommation inconnue</span>
        ) : (
          <span className="text-sm font-medium tabular-nums">
            {money(budget.spendUsd)}
            {budget.maxBudgetUsd != null && (
              <span className="text-muted"> / {money(budget.maxBudgetUsd)}</span>
            )}
          </span>
        )}
      </div>
      {ratio !== null && (
        <div className="h-1.5 w-full overflow-hidden rounded-full bg-surface-hover">
          <div
            className={`h-full rounded-full transition-all ${tone}`}
            style={{ width: `${Math.max(2, ratio * 100)}%` }}
          />
        </div>
      )}
      <p className="text-xs text-muted">
        {budget.maxBudgetUsd == null
          ? "Aucun plafond : la Virtual Key de ce Workshop n'est pas limitée en dépense."
          : "Au-delà du plafond, LiteLLM refuse les appels de ce Workshop."}
      </p>
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

  // Consommation LLM : information d'appoint, jamais bloquante — une
  // passerelle LiteLLM absente ou injoignable ne doit pas empecher
  // d'afficher un Workshop par ailleurs sain (voir `getLlmBudget`).
  // Credentials : liste des regles, jamais les valeurs. Non bloquant comme
  // le budget — une lecture qui echoue ne doit pas masquer le Workshop.
  let credentials: Awaited<ReturnType<typeof listCredentials>> = [];
  try {
    credentials = await listCredentials(name);
  } catch {
    credentials = [];
  }

  let budget: Awaited<ReturnType<typeof getLlmBudget>> = null;
  try {
    budget = await getLlmBudget(name);
  } catch {
    budget = null;
  }

  return (
    <>
      <LiveRefresh active={busy} />
      <TopNav />
      <main className="flex-1 max-w-2xl w-full mx-auto p-6 sm:p-8 flex flex-col gap-6">
        <div className="flex items-center justify-between gap-4">
          <div className="flex flex-col gap-1">
            <Link href="/workshops" className="text-sm text-muted hover:text-accent transition-colors">
              ← Workshops
            </Link>
            <h1 className="text-2xl font-semibold tracking-tight">{name}</h1>
          </div>
          <PhaseBadge phase={phase} size="md" />
        </div>

        <div className="grid grid-cols-2 gap-5 rounded-xl border border-border bg-surface p-5 shadow-sm">
          <Field
            label="Dépôt"
            value={maskUrlCredentials(workshop.spec.devcontainer.repo)}
          />
          <Field label="Révision" value={workshop.spec.devcontainer.revision} />
          <Field label="Pod parent" value={status?.podName ?? "—"} />
          <Field label="Image" value={status?.imageDigest ?? "—"} />
          <Field label="Snapshot" value={status?.snapshotDigest ?? "—"} />
        </div>

        {/* Confinement de securite (tache 4.2.4) : la PHASE reste `Running`
            — la microVM est deliberement conservee pour rester analysable —
            donc rien d'autre ne signalerait qu'un Workshop est coupe du
            reseau et son etat archive. En haut de page, avant tout le reste :
            c'est l'information qui change la lecture de toutes les autres. */}
        {status?.conditions?.SecurityLockdown === "true" && (
          <div className="rounded-xl border border-red-500/40 bg-red-500/5 p-4">
            <p className="text-sm font-semibold text-red-600 dark:text-red-400">
              Confinement de sécurité actif
            </p>
            <p className="mt-1 text-xs text-red-600/90 dark:text-red-400/90">
              Une anomalie réseau a été détectée : accès sortant gelé et état de
              la microVM archivé. Elle est conservée en l&apos;état pour analyse
              — la phase reste <code>Running</code> pour cette raison, elle ne
              signifie pas que le Workshop est utilisable.
            </p>
          </div>
        )}

        {budget && <LlmBudgetCard budget={budget} />}

        <Credentials workshopName={name} initial={credentials} />

        <div className="flex flex-wrap gap-2">
          {canConnect && (
            <ConnectLink
              href={`/workshops/${encodeURIComponent(name)}/vscode/`}
              label="Ouvrir VS Code"
              variant="primary"
            />
          )}
          {canConnect && (
            <ConnectLink
              href={`/workshops/${encodeURIComponent(name)}/terminal/`}
              label="Terminal"
              variant="secondary"
            />
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

        {canConnect && (
          <div className="flex flex-col gap-3">
            <h2 className="text-sm font-medium text-muted uppercase tracking-wide">Terminal</h2>
            <TerminalFrame src={`/workshops/${encodeURIComponent(name)}/terminal/`} />
          </div>
        )}
      </main>
    </>
  );
}
