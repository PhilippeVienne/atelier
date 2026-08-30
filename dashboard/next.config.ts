import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  skipTrailingSlashRedirect: true,
  // Le dashboard de dev est accede via l'ingress Traefik sous
  // app.atelier.local (deploy/dev/traefik/), pas localhost : sans ceci,
  // Next.js 16 bloque les requetes dev cross-origin par defaut (assets et
  // websocket HMR /_next/hmr inclus), voir next.config.ts d'origine.
  allowedDevOrigins: ["app.atelier.local"],
};

export default nextConfig;
