import Link from "next/link";
import type { ReactNode } from "react";
import { SessionKeepAlive } from "@/app/components/session-keepalive";
import { NavLink } from "@/app/components/nav-link";

// `className` optionnel : le chat plein ecran (app/page.tsx) a besoin que
// ce header ne prenne pas part au calcul de la hauteur scrollable (voir
// commentaire dans app/page.tsx) - les autres pages gardent le
// comportement par defaut (largeur max, page qui scroll normalement).
export function TopNav({ children, className }: { children?: ReactNode; className?: string }) {
  return (
    <header
      className={
        className ??
        "border-b border-border bg-surface/80 backdrop-blur supports-[backdrop-filter]:bg-surface/60 sticky top-0 z-10"
      }
    >
      <SessionKeepAlive />
      <div className="max-w-5xl mx-auto px-6 py-4 flex items-center justify-between gap-4">
        <div className="flex items-center gap-6">
          <Link href="/" className="flex items-center gap-2 font-semibold tracking-tight shrink-0">
            <span className="inline-flex h-7 w-7 items-center justify-center rounded-lg bg-accent text-accent-foreground text-sm font-bold">
              A
            </span>
            Atelier
          </Link>
          <nav className="flex items-center gap-1">
            <NavLink href="/">Chat</NavLink>
            <NavLink href="/projects">Projets</NavLink>
            <NavLink href="/workshops">Workshops</NavLink>
          </nav>
        </div>
        <div className="flex items-center gap-3">{children}</div>
      </div>
    </header>
  );
}
