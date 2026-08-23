"use client";

import { useEffect, useState } from "react";

// Le pod passe en phase `Running` des que la microVM a booté (voir
// `crates/controller/src/reconcile.rs`), pas une fois que `ttyd`/`code-server`
// ecoutent reellement dans le guest — ces services demarrent via systemd
// *apres* le boot du kernel, et `code-server` en particulier met du temps a
// se lier a son port. Sans ca, le premier clic sur "Terminal"/"Ouvrir VS
// Code" tombe sur un port pas encore ouvert : le retry cote
// `dashboard/server.ts` (websocket) ou l'echec rapide cote `api-server`
// (HTTP, connexion refusee) masquent ou exposent cette attente sans aucun
// retour visuel — poll ici cote client pour l'afficher explicitement au
// lieu de laisser l'utilisateur cliquer dans le vide.
const POLL_INTERVAL_MS = 1500;
// Au-dela, ce n'est plus un demarrage normal qui traine : autant arreter de
// laisser croire qu'un spinner suffit et donner la main a l'utilisateur
// (lien brut, qui exposera la vraie erreur s'il y en a une) plutot que de
// masquer indefiniment un echec reel.
const SLOW_AFTER_ATTEMPTS = 20; // ~30s

function useGuestServiceReady(url: string): { ready: boolean; slow: boolean } {
  const [ready, setReady] = useState(false);
  const [slow, setSlow] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    let attempts = 0;

    async function poll() {
      let ok: boolean;
      try {
        const res = await fetch(url, { method: "HEAD", cache: "no-store" });
        // `code-server` (VS Code) repond en 405 a un `HEAD` mais est bien
        // demarre et pret pour un `GET` — seul un 502/504 (api-server ne
        // peut pas joindre le guest) signifie que le service n'ecoute pas.
        ok = res.ok || res.status === 405 || (res.status >= 200 && res.status < 400);
      } catch {
        ok = false;
      }
      if (cancelled) return;
      if (ok) {
        setReady(true);
        return;
      }
      attempts += 1;
      if (attempts >= SLOW_AFTER_ATTEMPTS) setSlow(true);
      timer = setTimeout(poll, POLL_INTERVAL_MS);
    }

    poll();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [url]);

  return { ready, slow };
}

function Spinner() {
  return (
    <span
      className="inline-block h-3.5 w-3.5 rounded-full border-2 border-current border-t-transparent animate-spin"
      aria-hidden="true"
    />
  );
}

export function ConnectLink({
  href,
  label,
  variant,
}: {
  href: string;
  label: string;
  variant: "primary" | "secondary";
}) {
  const { ready, slow } = useGuestServiceReady(href);

  const className =
    variant === "primary"
      ? "rounded-full bg-accent text-accent-foreground px-4 py-2 text-sm font-medium hover:bg-accent-hover transition-colors"
      : "rounded-full border border-border px-4 py-2 text-sm font-medium hover:bg-surface-hover transition-colors";

  if (!ready) {
    return (
      <span
        className={`${className} inline-flex items-center gap-2 opacity-60 cursor-wait`}
        aria-busy="true"
      >
        <Spinner />
        {slow ? `${label} (demarrage plus long que prevu…)` : `${label}…`}
      </span>
    );
  }

  return (
    <a href={href} target="_blank" rel="noopener noreferrer" className={className}>
      {label} ↗
    </a>
  );
}

export function TerminalFrame({ src }: { src: string }) {
  const { ready, slow } = useGuestServiceReady(src);

  return (
    <div className="relative w-full h-[420px] rounded-xl border border-border bg-black overflow-hidden">
      {ready ? (
        <iframe src={src} title="Terminal" className="w-full h-full" />
      ) : (
        <div className="w-full h-full flex flex-col items-center justify-center gap-3 text-sm text-muted">
          <Spinner />
          <span>{slow ? "Demarrage plus long que prevu…" : "Demarrage du terminal…"}</span>
          {slow && (
            <a href={src} target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
              Ouvrir quand meme ↗
            </a>
          )}
        </div>
      )}
    </div>
  );
}
