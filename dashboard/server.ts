// Serveur Next.js custom : necessaire uniquement pour relayer le WebSocket
// "live" propre de `code-server`/`ttyd` (`/workshops/{name}/vscode/...` et
// `/workshops/{name}/terminal/...`) — un Route Handler standard
// (`app/workshops/[name]/vscode|terminal/[[...path]]/route.ts`, qui gere
// deja tous les assets HTTP normaux) ne peut pas hijacker une connexion pour
// un upgrade WebSocket. Voir `node_modules/next/dist/docs/01-app/02-guides/custom-server.md`.
//
// Le cookie de session (`atelier_session`, JWT Kanidm en clair — voir
// `lib/session.ts`) est lu ici directement dans les en-tetes de la requete
// d'upgrade : aucune API Next (`cookies()`) n'est disponible hors du
// contexte d'une requete geree par le framework.
import { createServer } from "node:http";
import next from "next";
import { WebSocket, WebSocketServer } from "ws";
import { API_SERVER_URL } from "./lib/config";

const port = parseInt(process.env.PORT ?? "3000", 10);
const dev = process.env.NODE_ENV !== "production";
const app = next({ dev });
const handle = app.getRequestHandler();

const SESSION_COOKIE = "atelier_session";
const UPGRADE_PATH = /^\/workshops\/([^/]+)\/(vscode|terminal)(\/.*)?$/;
const API_SERVER_WS_URL = API_SERVER_URL.replace(/^http/, "ws");

// La chaine navigateur -> dashboard -> api-server -> net-proxy -> guest
// comporte plusieurs sauts reseau reels ; un echec transitoire sur l'un
// d'eux (constate en pratique, notamment sous charge) ne doit pas se
// traduire par un `1006` immediat cote navigateur — `ttyd`/VS Code
// referaient alors toute leur propre reconnexion depuis zero, ce qui peut
// re-frapper la meme fenetre d'instabilite. Quelques tentatives rapprochees
// cote serveur, invisibles pour le client, absorbent ce genre de blip.
const MAX_UPSTREAM_ATTEMPTS = 3;
const RETRY_DELAY_MS = 250;

function readCookie(header: string | undefined, name: string): string | undefined {
  if (!header) return undefined;
  for (const part of header.split(";")) {
    const separator = part.indexOf("=");
    if (separator === -1) continue;
    if (part.slice(0, separator).trim() === name) {
      return decodeURIComponent(part.slice(separator + 1).trim());
    }
  }
  return undefined;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

class UpstreamHttpError extends Error {
  constructor(
    public statusCode: number,
    public statusMessage: string,
  ) {
    super(`unexpected-response ${statusCode}`);
  }
}

/**
 * Ouvre la connexion websocket amont vers `api-server`, avec quelques
 * tentatives rapprochees en cas d'echec reseau transitoire (`error`, pas
 * une vraie reponse HTTP) — une reponse HTTP explicite (`unexpected-response`,
 * ex. 401/404) n'est en revanche jamais retentee : ce n'est pas un blip,
 * retenter donnerait le meme resultat.
 */
async function openUpstream(
  targetUrl: string,
  protocols: string[] | undefined,
  token: string,
  isAborted: () => boolean,
): Promise<WebSocket> {
  let lastError: unknown;
  for (let attempt = 1; attempt <= MAX_UPSTREAM_ATTEMPTS; attempt++) {
    if (isAborted()) throw new Error("navigateur deconnecte avant la fin de la tentative");
    try {
      return await new Promise<WebSocket>((resolve, reject) => {
        const ws = new WebSocket(targetUrl, protocols, {
          headers: { Authorization: `Bearer ${token}` },
        });
        ws.once("open", () => {
          resolve(ws);
        });
        ws.once("unexpected-response", (_req, res) => {
          reject(new UpstreamHttpError(res.statusCode ?? 502, res.statusMessage || "Bad Gateway"));
        });
        ws.once("error", (err) => {
          reject(err);
        });
      });
    } catch (err) {
      if (err instanceof UpstreamHttpError) throw err;
      lastError = err;
      if (attempt < MAX_UPSTREAM_ATTEMPTS) await sleep(RETRY_DELAY_MS);
    }
  }
  throw lastError;
}

app.prepare().then(() => {
  // Next installe *lui-meme* un listener `upgrade` sur notre serveur des la
  // premiere requete HTTP servie : `getRequestHandler()` appelle
  // `setupWebSocketHandler()` qui recupere le serveur via `req.socket.server`
  // et s'y accroche (`node_modules/next/dist/server/next.js`). Or son handler
  // (`node_modules/next/dist/server/lib/router-server.js`) fait
  // `if (matchedOutput) return socket.end()` : nos chemins d'upgrade
  // correspondent au Route Handler catch-all
  // `app/workshops/[name]/(vscode|terminal)/[[...path]]/route.ts`, donc Next
  // fermait la socket du navigateur en parallele de notre propre handler —
  // `1006` cote client, de facon parfaitement deterministe mais seulement
  // *apres* la premiere requete HTTP du process (d'ou une apparence de
  // flakiness). On neutralise cet auto-attachement en declenchant le setup
  // nous-memes sans serveur cible, et on delegue explicitement a
  // `app.getUpgradeHandler()` (API publique) tout ce qui ne nous concerne
  // pas, pour que le HMR de Next continue de fonctionner.
  (app as unknown as { setupWebSocketHandler: () => void }).setupWebSocketHandler();
  const handleNextUpgrade = app.getUpgradeHandler();

  const server = createServer((req, res) => handle(req, res));

  server.on("upgrade", (req, socket, head) => {
    const match = UPGRADE_PATH.exec(req.url ?? "");
    const token = readCookie(req.headers.cookie, SESSION_COOKIE);
    if (!match) {
      // Pas un upgrade "guest" : c'est le HMR de Next (ou rien du tout).
      void handleNextUpgrade(req, socket, head);
      return;
    }
    if (!token) {
      socket.destroy();
      return;
    }
    const [, name, service, rest] = match;
    const targetUrl = `${API_SERVER_WS_URL}/v1/workshops/${name}/${service}${rest ?? "/"}`;

    // Sous-protocole WebSocket (`Sec-WebSocket-Protocol`) : `ttyd` en exige
    // un precis (`tty`, negocie explicitement par son client JS, voir
    // `new WebSocket(url, ["tty"])`) — un navigateur qui a demande un
    // sous-protocole ferme la connexion (code 1006) si la reponse `101` ne
    // le confirme pas. On relaie donc tel quel ce que le navigateur a
    // demande vers l'amont, puis on repond au navigateur avec exactement
    // ce que l'amont a lui-meme negocie (`upstream.protocol`) plutot que de
    // deviner : une nouvelle `WebSocketServer` par requete (leger, `noServer:true`)
    // permet de fixer `handleProtocols` avec cette valeur connue au moment
    // de l'upgrade.
    const requestedProtocols = req.headers["sec-websocket-protocol"]
      ?.split(",")
      .map((p) => p.trim())
      .filter(Boolean);

    let browserGone = false;
    socket.once("close", () => {
      browserGone = true;
    });

    // Ouvre d'abord la connexion amont (vers api-server) et attend sa
    // confirmation *avant* de repondre `101` au navigateur — envoyer ce
    // `101` plus tot serait inconditionnel, sans savoir si l'amont a
    // reellement accepte l'upgrade. Bug constate en testant reellement : un
    // navigateur recevait un `101` "reussi" meme quand la connexion vers
    // api-server echouait ensuite (le canal restait mort en silence). Un
    // client WebSocket standard n'envoie de toute facon aucune trame avant
    // d'avoir recu son propre `101`, donc rien n'est perdu a attendre ici.
    openUpstream(targetUrl, requestedProtocols, token, () => browserGone)
      .then((upstream) => {
        if (browserGone) {
          upstream.close();
          return;
        }
        const wss = new WebSocketServer({
          noServer: true,
          handleProtocols: () => upstream.protocol || false,
        });
        wss.handleUpgrade(req, socket, head, (browserSocket) => {
          browserSocket.on("message", (data, isBinary) => {
            upstream.send(data, { binary: isBinary });
          });
          upstream.on("message", (data, isBinary) => {
            browserSocket.send(data, { binary: isBinary });
          });

          const closeBoth = () => {
            if (browserSocket.readyState === WebSocket.OPEN) browserSocket.close();
            if (upstream.readyState === WebSocket.OPEN) upstream.close();
          };
          browserSocket.on("close", closeBoth);
          browserSocket.on("error", closeBoth);
          upstream.on("close", closeBoth);
          upstream.on("error", closeBoth);
        });
      })
      .catch((err) => {
        if (socket.destroyed) return;
        if (err instanceof UpstreamHttpError) {
          // `end()`, pas `write()` + `destroy()` : `destroy()` immediatement
          // apres `write()` peut fermer la socket avant que l'ecriture ne
          // soit effectivement envoyee (constate en pratique : le
          // navigateur ne recevait rien du tout). `end()` ecrit puis ferme
          // proprement une fois le buffer vide.
          socket.end(`HTTP/1.1 ${err.statusCode} ${err.statusMessage}\r\n\r\n`);
          return;
        }
        console.error(
          `connexion amont ${service} (websocket) echouee apres ${MAX_UPSTREAM_ATTEMPTS} tentatives:`,
          err,
        );
        socket.end("HTTP/1.1 502 Bad Gateway\r\n\r\n");
      });
  });

  server.listen(port, () => {
    console.log(`> dashboard en ecoute sur http://localhost:${port} (${dev ? "dev" : "production"})`);
  });
});
