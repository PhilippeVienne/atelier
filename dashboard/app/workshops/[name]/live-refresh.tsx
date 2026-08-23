"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";

/**
 * Pendant une phase transitoire (build d'image, provisioning, ...), la
 * seule facon de voir la progression sans que l'utilisateur rafraichisse
 * lui-meme la page est de re-executer le Server Component periodiquement.
 * S'arrete de lui-meme des que `active` devient faux (phase stabilisee).
 */
export function LiveRefresh({ active }: { active: boolean }) {
  const router = useRouter();

  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => router.refresh(), 3000);
    return () => clearInterval(id);
  }, [active, router]);

  return null;
}
