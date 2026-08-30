import "server-only";
import { FORGEJO_ADMIN_TOKEN, FORGEJO_URL } from "./config";

// Client Forgejo (API admin, `token <ATELIER_FORGEJO_ADMIN_TOKEN>`) pour la
// fonctionnalite "Projets" : miroir en lecture d'un depot GitHub/GitLab
// (prive ou public) vers cette instance Forgejo interne, pour que le reste
// de l'automatisation (PM Engine `ForgejoProvider`, creation de Workshop)
// ne parle plus jamais directement a un service externe. Le credential
// externe (PAT) n'est transmis qu'une seule fois, ici, a l'appel de
// creation : Forgejo le persiste lui-meme cote serveur pour la
// resynchronisation periodique du miroir (`mirror_interval`) — ce module ne
// le stocke ni ne le journalise.
export const FORGEJO_OWNER = process.env.ATELIER_FORGEJO_OWNER ?? "atelier_admin";

export class ForgejoError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = "ForgejoError";
  }
}

export function forgejoMirrorEnabled(): boolean {
  return Boolean(FORGEJO_ADMIN_TOKEN);
}

async function call(path: string, init: RequestInit = {}): Promise<Response> {
  if (!FORGEJO_ADMIN_TOKEN) {
    throw new ForgejoError(503, "miroir Forgejo non configure (ATELIER_FORGEJO_ADMIN_TOKEN absent)");
  }
  const res = await fetch(`${FORGEJO_URL}/api/v1${path}`, {
    ...init,
    headers: {
      ...init.headers,
      Authorization: `token ${FORGEJO_ADMIN_TOKEN}`,
    },
    cache: "no-store",
  });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new ForgejoError(res.status, body || res.statusText);
  }
  return res;
}

export interface MirrorProject {
  name: string;
  fullName: string;
  owner: string;
  cloneUrl: string;
  originalUrl: string | null;
  private: boolean;
  updatedAt: string;
}

interface ForgejoRepo {
  name: string;
  full_name: string;
  owner: { login: string };
  clone_url: string;
  original_url: string | null;
  private: boolean;
  updated_at: string;
  mirror: boolean;
}

// Liste les depots de `FORGEJO_OWNER` marques `mirror: true` : ce sont
// exclusivement les "Projets" crees via `createMirrorProject` ci-dessous
// (les Workshops eux-memes ne creent jamais de depot Forgejo).
export async function listMirrorProjects(): Promise<MirrorProject[]> {
  const res = await call(
    `/repos/search?owner=${encodeURIComponent(FORGEJO_OWNER)}&limit=50&sort=updated&order=desc`,
  );
  const { data } = (await res.json()) as { data: ForgejoRepo[] };
  return data
    .filter((repo) => repo.mirror)
    .map((repo) => ({
      name: repo.name,
      fullName: repo.full_name,
      owner: repo.owner.login,
      cloneUrl: repo.clone_url,
      originalUrl: repo.original_url,
      private: repo.private,
      updatedAt: repo.updated_at,
    }));
}

// Determine le `service`/mode d'authentification Forgejo attendu par
// `POST /repos/migrate` a partir de l'hote source : GitHub attend un jeton
// nu (`auth_token`), GitLab (SaaS ou instance privee auto-hebergee, d'ou le
// simple test de sous-chaine plutot qu'un hote fixe) une authentification
// basique avec `oauth2` comme utilisateur, convention documentee de
// l'authentification HTTPS par jeton de GitLab.
function migrationAuth(
  sourceUrl: string,
  token: string | undefined,
): Pick<Record<string, string>, never> & {
  service: string;
  auth_token?: string;
  auth_username?: string;
  auth_password?: string;
} {
  let host = "";
  try {
    host = new URL(sourceUrl).host.toLowerCase();
  } catch {
    // URL invalide : laisse `service: "git"` generique, Forgejo renverra
    // lui-meme une erreur explicite au moment du clone si besoin.
  }
  if (host === "github.com" || host.endsWith(".github.com")) {
    return { service: "github", ...(token ? { auth_token: token } : {}) };
  }
  if (host.includes("gitlab")) {
    return { service: "gitlab", ...(token ? { auth_username: "oauth2", auth_password: token } : {}) };
  }
  return { service: "git", ...(token ? { auth_username: "git", auth_password: token } : {}) };
}

export interface CreateMirrorProjectInput {
  name: string;
  sourceUrl: string;
  private: boolean;
  token?: string;
}

export async function createMirrorProject(input: CreateMirrorProjectInput): Promise<MirrorProject> {
  const res = await call("/repos/migrate", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      clone_addr: input.sourceUrl,
      repo_name: input.name,
      repo_owner: FORGEJO_OWNER,
      mirror: true,
      private: input.private,
      // Resynchronisation automatique : un projet miroir sans intervalle
      // explicite ne se mettrait jamais a jour tout seul, contrairement a
      // l'attente d'un "miroir".
      mirror_interval: "10m0s",
      ...migrationAuth(input.sourceUrl, input.token),
    }),
  });
  const repo = (await res.json()) as ForgejoRepo;
  return {
    name: repo.name,
    fullName: repo.full_name,
    owner: repo.owner.login,
    cloneUrl: repo.clone_url,
    originalUrl: repo.original_url,
    private: repo.private,
    updatedAt: repo.updated_at,
  };
}
