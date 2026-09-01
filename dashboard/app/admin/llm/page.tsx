import Link from "next/link";
import { notFound } from "next/navigation";
import { TopNav } from "@/app/components/top-nav";
import { logout } from "@/app/actions";
import {
  ApiServerError,
  getLlmOverview,
  getSpendReport,
  type LlmKey,
  type SpendBucket,
  type SpendReport,
} from "@/lib/api-server";
import { getCurrentUser } from "@/lib/session";

// Console d'administration de la passerelle LiteLLM.
//
// L'autorisation réelle est faite par l'api-server (rôle `admin`, `403`
// sinon) : le contrôle ci-dessous ne sert qu'à présenter un 404 plutôt
// qu'une page en erreur à qui n'y a pas droit. Ne jamais s'y fier seul.
export const dynamic = "force-dynamic";

/** Deux echelles distinctes : une Virtual Key depense des fractions de
 *  centime (4 decimales, sinon tout s'affiche a « 0,00 $ »), un total en
 *  cumule des centaines (2 decimales, sinon on lit « 211,4748 $ » la ou la
 *  precision n'apporte rien). */
const money = (v: number, precise = false) =>
  v.toLocaleString("fr-FR", {
    style: "currency",
    currency: "USD",
    currencyDisplay: "narrowSymbol",
    minimumFractionDigits: 2,
    maximumFractionDigits: precise ? 4 : 2,
  });

/** Repartition d'une depense, en barres proportionnelles au plus gros
 *  poste. Un tableau de chiffres se lit ligne a ligne ; ce qu'on veut voir
 *  ici, c'est LEQUEL coute, d'un coup d'oeil. */
function Breakdown({
  title,
  buckets,
  empty,
}: {
  title: string;
  buckets: SpendBucket[];
  empty: string;
}) {
  const max = Math.max(...buckets.map((b) => b.spendUsd), 0);
  return (
    <div className="flex flex-col gap-2">
      <h3 className="text-xs uppercase tracking-wide text-muted">{title}</h3>
      {buckets.length === 0 ? (
        <p className="text-sm text-muted">{empty}</p>
      ) : (
        <ul className="flex flex-col gap-1.5">
          {buckets.map((b) => (
            <li key={b.label} className="flex flex-col gap-1">
              <div className="flex items-baseline justify-between gap-3">
                <span className="truncate font-mono text-xs">{b.label}</span>
                <span className="shrink-0 tabular-nums text-xs">
                  {money(b.spendUsd, true)}
                  <span className="ml-2 text-muted">{b.requestCount} req.</span>
                </span>
              </div>
              <div className="h-1 rounded-full bg-surface-hover">
                <div
                  className="h-1 rounded-full bg-accent"
                  style={{ width: `${max > 0 ? (b.spendUsd / max) * 100 : 0}%` }}
                />
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function SpendPanel({ report }: { report: SpendReport }) {
  return (
    <section className="rounded-xl border border-border bg-surface/70 p-4 flex flex-col gap-5">
      <div>
        <h2 className="text-sm font-semibold">Dépense</h2>
        <p className="mt-1 text-xs text-muted">
          Agrégée depuis les journaux de LiteLLM. Le rapport équivalent côté
          LiteLLM (<code>/global/spend/report</code>) est réservé à son
          édition Enterprise.
        </p>
      </div>

      <div className="grid gap-3 sm:grid-cols-3">
        <div>
          <p className="text-[11px] uppercase tracking-wide text-muted">Dépense réelle</p>
          <p className="mt-1 text-lg font-semibold tabular-nums">
            {money(report.totalUsd, true)}
          </p>
        </div>
        <div>
          <p className="text-[11px] uppercase tracking-wide text-muted">Non rattachée</p>
          <p className="mt-1 text-lg font-semibold tabular-nums">
            {money(report.unattributedUsd, true)}
          </p>
          <p className="mt-1 text-xs text-muted">
            {report.totalUsd > 0
              ? `${Math.round((report.unattributedUsd / report.totalUsd) * 100)} % du total. `
              : ""}
            Passée par le jeton partagé : aucun plafond de Workshop ne la
            gouverne.
          </p>
        </div>
        {report.testPricingUsd > 0 && (
          <div>
            <p className="text-[11px] uppercase tracking-wide text-muted">
              Tarif fictif, écarté
            </p>
            <p className="mt-1 text-lg font-semibold tabular-nums text-muted">
              {money(report.testPricingUsd)}
            </p>
            <p className="mt-1 text-xs text-muted">
              Modèles de test facturés des dollars par requête, pour exercer
              les plafonds. Hors total.
            </p>
          </div>
        )}
      </div>

      <div className="grid gap-6 sm:grid-cols-3">
        <div className="flex flex-col gap-2">
          <Breakdown
            title="Par groupe"
            buckets={report.byGroup}
            empty="Aucune dépense rattachée à un groupe."
          />
          {report.byGroup.length > 0 && (
            <p className="text-xs text-muted">
              Les clés émises avant le modèle par groupe portent un sujet OIDC
              à la place : un identifiant opaque ici est une clé de cette
              époque, pas un groupe inconnu.
            </p>
          )}
        </div>
        <Breakdown title="Par jour" buckets={report.byDay} empty="Aucune dépense." />
        <Breakdown title="Par modèle" buckets={report.byModel} empty="Aucune dépense." />
      </div>
    </section>
  );
}

function KeyRow({ k }: { k: LlmKey }) {
  const ratio =
    k.maxBudgetUsd && k.maxBudgetUsd > 0 ? Math.min(1, k.spendUsd / k.maxBudgetUsd) : null;
  return (
    <tr className={k.expired ? "text-muted" : ""}>
      <td className="py-2 pr-4 font-mono text-xs break-all">{k.alias}</td>
      <td className="py-2 pr-4 tabular-nums">{money(k.spendUsd, true)}</td>
      <td className="py-2 pr-4 tabular-nums">
        {k.maxBudgetUsd == null ? (
          <span className="text-muted">aucun</span>
        ) : (
          <span className="inline-flex items-center gap-2">
            {money(k.maxBudgetUsd)}
            {ratio !== null && (
              <span className="inline-block h-1 w-12 overflow-hidden rounded-full bg-surface-hover align-middle">
                <span
                  className={`block h-full rounded-full ${
                    ratio >= 0.8 ? "bg-red-500" : ratio >= 0.5 ? "bg-amber-500" : "bg-emerald-500"
                  }`}
                  style={{ width: `${Math.max(4, ratio * 100)}%` }}
                />
              </span>
            )}
          </span>
        )}
      </td>
      <td className="py-2 pr-4 font-mono text-xs">
        {k.owner ? `${k.owner.slice(0, 8)}…` : "—"}
      </td>
      <td className="py-2">
        <span
          className={`rounded-full px-2 py-0.5 text-xs font-medium ${
            k.expired
              ? "bg-neutral-500/10 text-muted"
              : "bg-emerald-500/10 text-emerald-700 dark:text-emerald-400"
          }`}
        >
          {k.expired ? "expirée" : "active"}
        </span>
      </td>
    </tr>
  );
}

export default async function AdminLlmPage() {
  const user = await getCurrentUser();
  if (!user?.roles.includes("admin")) notFound();

  let overview;
  let spend: SpendReport | null = null;
  try {
    overview = await getLlmOverview();
    spend = await getSpendReport();
  } catch (err) {
    // Un 403 ne devrait pas arriver ici (le garde ci-dessus l'a filtré), mais
    // l'api-server reste seul juge : s'il refuse, on présente la même absence
    // plutôt qu'une trace d'erreur.
    if (err instanceof ApiServerError && (err.status === 403 || err.status === 503)) notFound();
    throw err;
  }

  const active = overview.keys.filter((k) => !k.expired);
  const activeSpend = active.reduce((sum, k) => sum + k.spendUsd, 0);
  // Les cles expirees s'accumulent (TTL court, une paire par Workshop et par
  // reprise) : sur une instance un peu vecue elles noient tout le reste de la
  // page. On garde celles qui ont coute quelque chose — c'est la seule raison
  // de regarder une cle morte — et on annonce le nombre des autres.
  const EXPIRED_SHOWN = 15;
  const expired = overview.keys.filter((k) => k.expired);
  const expiredShown = expired
    .filter((k) => k.spendUsd > 0)
    .concat(expired.filter((k) => k.spendUsd === 0))
    .slice(0, EXPIRED_SHOWN);
  const shownKeys = [...active, ...expiredShown];
  const hiddenExpired = expired.length - expiredShown.length;

  return (
    <div className="min-h-dvh flex flex-col">
      <TopNav>
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

      <main className="mx-auto w-full max-w-5xl px-4 py-6 flex flex-col gap-6">
        <div>
          <h1 className="text-xl font-semibold">Passerelle LLM</h1>
          <p className="mt-1 text-sm text-muted">
            État de LiteLLM : modèles servis et Virtual Keys émises pour les
            Workshops. Les jetons eux-mêmes ne sont jamais exposés ici.
          </p>
        </div>

        <section className="grid gap-3 sm:grid-cols-3">
          <div className="rounded-xl border border-border bg-surface/70 p-4">
            <p className="text-[11px] uppercase tracking-wide text-muted">
              Compteur LiteLLM
            </p>
            <p className="mt-1 text-lg font-semibold tabular-nums">
              {overview.globalSpendUsd == null ? "—" : money(overview.globalSpendUsd)}
            </p>
            <p className="mt-1 text-xs text-muted">
              Total brut de LiteLLM, toutes clés confondues. Il inclut les
              modèles à tarif fictif : la dépense réelle est plus bas.
            </p>
          </div>
          <div className="rounded-xl border border-border bg-surface/70 p-4">
            <p className="text-[11px] uppercase tracking-wide text-muted">Clés actives</p>
            <p className="mt-1 text-lg font-semibold tabular-nums">{active.length}</p>
            <p className="mt-1 text-xs text-muted">
              {money(activeSpend)} dépensés sur ces clés.
            </p>
          </div>
          <div className="rounded-xl border border-border bg-surface/70 p-4">
            <p className="text-[11px] uppercase tracking-wide text-muted">Modèles servis</p>
            <p className="mt-1 text-lg font-semibold tabular-nums">{overview.models.length}</p>
            <p className="mt-1 text-xs text-muted">Alias exposés aux Workshops.</p>
          </div>
        </section>

        {spend && <SpendPanel report={spend} />}

        <section className="rounded-xl border border-border bg-surface/70 p-4">
          <h2 className="text-sm font-semibold">Modèles</h2>
          <div className="mt-3 overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-left text-xs uppercase tracking-wide text-muted">
                  <th className="pb-2 pr-4 font-normal">Alias</th>
                  <th className="pb-2 pr-4 font-normal">Modèle réel</th>
                  <th className="pb-2 font-normal">Endpoint</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {overview.models.map((m) => (
                  <tr key={m.name}>
                    <td className="py-2 pr-4 font-mono text-xs">{m.name}</td>
                    <td className="py-2 pr-4 font-mono text-xs">{m.target ?? "—"}</td>
                    <td className="py-2 font-mono text-xs text-muted break-all">
                      {m.apiBase ?? "défaut du fournisseur"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>

        <section className="rounded-xl border border-border bg-surface/70 p-4">
          <h2 className="text-sm font-semibold">Virtual Keys</h2>
          <p className="mt-1 text-xs text-muted">
            Une clé par Workshop (<code>atelier-wks-…</code>) et une par
            construction d&apos;image (<code>atelier-build-…</code>), à TTL
            court. Les 100 plus récentes.
          </p>
          {overview.keys.length === 0 ? (
            <p className="mt-4 text-sm text-muted">Aucune clé émise pour l&apos;instant.</p>
          ) : (
            <div className="mt-3 overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="text-left text-xs uppercase tracking-wide text-muted">
                    <th className="pb-2 pr-4 font-normal">Alias</th>
                    <th className="pb-2 pr-4 font-normal">Dépense</th>
                    <th className="pb-2 pr-4 font-normal">Plafond</th>
                    <th className="pb-2 pr-4 font-normal">Propriétaire</th>
                    <th className="pb-2 font-normal">État</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border">
                  {shownKeys.map((k) => (
                    <KeyRow key={k.alias} k={k} />
                  ))}
                </tbody>
              </table>
              {hiddenExpired > 0 && (
                <p className="mt-3 text-xs text-muted">
                  {hiddenExpired} clé{hiddenExpired > 1 ? "s" : ""} expirée
                  {hiddenExpired > 1 ? "s" : ""} de plus, non affichée
                  {hiddenExpired > 1 ? "s" : ""} — elles ne consomment plus
                  rien. La dépense qu&apos;elles ont portée reste comptée dans
                  le panneau « Dépense » ci-dessus.
                </p>
              )}
            </div>
          )}
        </section>

        <p className="text-xs text-muted">
          La dépense d&apos;un Workshop est imputée à sa Virtual Key dédiée :
          l&apos;alias <code>llm-proxy</code> traverse <code>identity-proxy</code>,
          qui substitue la clé du Workshop au jeton du guest. Les appels
          antérieurs à ce câblage restent, eux, imputés au jeton statique
          partagé — d&apos;où un total global supérieur à la somme des clés.
        </p>
      </main>
    </div>
  );
}
