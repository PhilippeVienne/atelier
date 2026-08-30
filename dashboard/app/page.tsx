import Link from "next/link";
import { TopNav } from "@/app/components/top-nav";
import { logout } from "@/app/actions";
import { listPendingReviews, PmEngineError } from "@/lib/pm-engine";
import { forgejoMirrorEnabled, listProjects } from "@/lib/forgejo";
import { PmChat } from "./pm/pm-chat";

// Page d'accueil du Dashboard : le chat PM Engine (Jalon M5, tache 5.5.1)
// est l'element principal de l'UI, pas la liste des Workshops (deplacee
// sur /workshops). Plein ecran (h-dvh + overflow-hidden) plutot que la
// page qui scroll normalement des autres routes : seule la liste de
// messages doit defiler, la barre de saisie reste fixee en bas.
export default async function HomePage() {
  let pendingCount = 0;
  try {
    pendingCount = (await listPendingReviews()).length;
  } catch (err) {
    // Non bloquant pour le chat lui-meme : le badge de revues disparait
    // juste si le PM Engine est injoignable au chargement de la page.
    if (!(err instanceof PmEngineError)) throw err;
  }

  // Projets reellement importes (miroirs Forgejo, voir /projects) : le chat
  // en propose la liste plutot que d'attendre un identifiant saisi a la
  // main. Non bloquant lui aussi — sans miroir configure, le chat reste
  // utilisable pour des questions generales.
  let projects: string[] = [];
  if (forgejoMirrorEnabled()) {
    try {
      projects = (await listProjects()).map((p) => p.fullName);
    } catch {
      projects = [];
    }
  }

  return (
    <div className="h-dvh flex flex-col overflow-hidden">
      <TopNav className="border-b border-border bg-surface/80 backdrop-blur supports-[backdrop-filter]:bg-surface/60">
        <Link
          href="/pm"
          className="relative text-sm text-muted hover:text-foreground transition-colors px-2"
        >
          Revues
          {pendingCount > 0 && (
            <span className="ml-1.5 inline-flex items-center justify-center rounded-full bg-accent text-accent-foreground text-[11px] font-medium h-4 min-w-4 px-1">
              {pendingCount}
            </span>
          )}
        </Link>
        <form action={logout}>
          <button className="text-sm text-muted hover:text-foreground transition-colors px-2">
            Se deconnecter
          </button>
        </form>
      </TopNav>
      <PmChat projects={projects} />
    </div>
  );
}
