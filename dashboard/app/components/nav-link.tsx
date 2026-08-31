"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import type { ReactNode } from "react";

// Surligne le lien correspondant a la route active dans TopNav.
// "/" ne doit matcher QUE la racine exacte (sinon il resterait actif sur
// toutes les routes, /workshops inclus, puisque tout chemin commence par
// "/") ; les autres prefixes matchent aussi leurs sous-routes
// (/workshops/[name], /workshops/new).
export function NavLink({ href, children }: { href: string; children: ReactNode }) {
  const pathname = usePathname();
  const active = href === "/" ? pathname === "/" : pathname.startsWith(href);

  return (
    <Link
      href={href}
      className={`shrink-0 whitespace-nowrap text-sm px-3 py-1.5 rounded-full transition-colors ${
        active
          ? "bg-accent/10 text-accent font-medium"
          : "text-muted hover:text-foreground hover:bg-surface-hover"
      }`}
    >
      {children}
    </Link>
  );
}
