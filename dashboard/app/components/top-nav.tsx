import Link from "next/link";
import type { ReactNode } from "react";
import { SessionKeepAlive } from "@/app/components/session-keepalive";
import { NavLink } from "@/app/components/nav-link";
import { getCurrentUser } from "@/lib/session";

// `className` optionnel : le chat plein ecran (app/page.tsx) a besoin que
// ce header ne prenne pas part au calcul de la hauteur scrollable (voir
// commentaire dans app/page.tsx) - les autres pages gardent le
// comportement par defaut (largeur max, page qui scroll normalement).
export async function TopNav({
  children,
  className,
}: {
  children?: ReactNode;
  className?: string;
}) {
  // Entree reservee aux administrateurs. Purement cosmetique : c'est
  // l'api-server qui refuse la route aux autres (`403`), masquer un lien
  // n'autorise ni n'interdit rien.
  const user = await getCurrentUser();
  const isAdmin = user?.roles.includes("admin") ?? false;
  return (
    <header
      className={
        className ??
        "border-b border-border bg-surface/80 backdrop-blur supports-[backdrop-filter]:bg-surface/60 sticky top-0 z-10"
      }
    >
      <SessionKeepAlive />
      {/* `min-w-0` + `overflow-x-auto` sur la barre de liens : sans eux, les
          quatre entrees, le logo et les actions ne tiennent pas sur un
          telephone et elargissent TOUTE la page (debordement horizontal
          constate a 390 px sur chaque route). Les liens defilent
          lateralement plutot que de pousser le reste, et `shrink-0` empeche
          le logo et les actions d'etre ecrases. */}
      <div className="max-w-5xl mx-auto px-4 sm:px-6 py-4 flex items-center justify-between gap-3 sm:gap-4">
        <div className="flex items-center gap-4 sm:gap-6 min-w-0">
          <Link href="/" className="flex items-center gap-2 font-semibold tracking-tight shrink-0">
            <span className="inline-flex h-7 w-7 items-center justify-center rounded-lg bg-accent text-accent-foreground text-sm font-bold">
              A
            </span>
            <span className="hidden sm:inline">Atelier</span>
          </Link>
          <nav className="flex items-center gap-1 min-w-0 overflow-x-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
            <NavLink href="/">Chat</NavLink>
            <NavLink href="/pipeline">Pipeline</NavLink>
            <NavLink href="/projects">Projets</NavLink>
            <NavLink href="/workshops">Workshops</NavLink>
            {isAdmin && <NavLink href="/admin/llm">LLM</NavLink>}
          </nav>
        </div>
        <div className="flex items-center gap-3 shrink-0">{children}</div>
      </div>
    </header>
  );
}
