"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";

// Le JWT du fournisseur OIDC (Keycloak en dev, defaut realm "atelier" :
// access token de courte duree de vie) expire rapidement — sans ca, une
// session de terminal/VS Code ouverte plus longtemps se met a echouer en
// boucle silencieuse (WebSocket 1006, indiscernable cote navigateur d'un
// vrai probleme reseau, cf. session de debug reelle). Rafraichi ici bien
// avant l'expiration (`/api/auth/refresh`, qui echange le refresh token
// cote serveur — jamais expose au JS) : tant que l'onglet reste ouvert et
// que le refresh token lui-meme reste valide, l'utilisateur ne voit jamais
// d'expiration. Monte une seule fois via `TopNav` (present sur toutes les
// pages protegees, jamais sur /login).
const REFRESH_INTERVAL_MS = 4 * 60 * 1000; // 4 min, marge large sous la duree de vie type d'un access token OIDC (5-15 min)

export function SessionKeepAlive() {
  const router = useRouter();

  useEffect(() => {
    const tick = async () => {
      try {
        const res = await fetch("/api/auth/refresh", { method: "POST", cache: "no-store" });
        if (res.status === 401) {
          router.replace("/login");
        }
      } catch {
        // Reseau transitoire : on retentera au prochain intervalle, pas la
        // peine de deconnecter l'utilisateur pour un blip.
      }
    };
    const id = setInterval(tick, REFRESH_INTERVAL_MS);

    // Les navigateurs limitent fortement les `setInterval` d'un onglet en
    // arriere-plan (throttling) : un onglet reste cache plus longtemps que
    // le token ne vit (15 min) reviendrait au meme probleme au retour au
    // premier plan sans ce rattrapage immediat.
    const onVisible = () => {
      if (document.visibilityState === "visible") tick();
    };
    document.addEventListener("visibilitychange", onVisible);

    return () => {
      clearInterval(id);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [router]);

  return null;
}
