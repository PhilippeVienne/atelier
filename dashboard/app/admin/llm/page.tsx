import Link from "next/link";
import { notFound } from "next/navigation";
import { TopNav } from "@/app/components/top-nav";
import { logout } from "@/app/actions";
import { ApiServerError, getLlmOverview, type LlmKey } from "@/lib/api-server";
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
  try {
    overview = await getLlmOverview();
  } catch (err) {
    // Un 403 ne devrait pas arriver ici (le garde ci-dessus l'a filtré), mais
    // l'api-server reste seul juge : s'il refuse, on présente la même absence
    // plutôt qu'une trace d'erreur.
    if (err instanceof ApiServerError && (err.status === 403 || err.status === 503)) notFound();
    throw err;
  }

  const active = overview.keys.filter((k) => !k.expired);
  const activeSpend = active.reduce((sum, k) => sum + k.spendUsd, 0);

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
            <p className="text-[11px] uppercase tracking-wide text-muted">Dépense totale</p>
            <p className="mt-1 text-lg font-semibold tabular-nums">
              {overview.globalSpendUsd == null ? "—" : money(overview.globalSpendUsd)}
            </p>
            <p className="mt-1 text-xs text-muted">
              Toutes clés confondues, jeton statique partagé inclus.
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
                  {overview.keys.map((k) => (
                    <KeyRow key={k.alias} k={k} />
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>

        {/* Limite connue, dite ici plutot que laissee a deviner : sans elle,
            un administrateur conclurait a tort que les Workshops ne
            consomment presque rien. */}
        <p className="rounded-xl border border-amber-500/30 bg-amber-500/5 p-4 text-xs text-amber-700 dark:text-amber-400">
          Limite connue : la dépense de l&apos;agent n&apos;est pas encore
          imputée à la clé de son Workshop. L&apos;alias <code>llm-proxy</code>{" "}
          ne traverse pas <code>identity-proxy</code>, si bien que le guest
          utilise le jeton statique partagé — les plafonds par Workshop ne
          contraignent donc rien aujourd&apos;hui. Voir{" "}
          <code>docs/architecture/pieges.md</code>.
        </p>
      </main>
    </div>
  );
}
