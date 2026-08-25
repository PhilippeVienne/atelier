variable "cluster_name" {
  description = "Utilise uniquement pour taguer les depots ECR (aucune dependance fonctionnelle envers modules/cluster)."
  type        = string
  default     = "atelier"
}

# Un depot ECR par image, mutable (":dev"/"latest" sont reutilisees a
# chaque build/mirroring, contrairement a un tag immuable par version) et
# scanne au push (CVE). Voir deploy/terraform/aws/mirror-images.sh pour le
# detail de ce qui alimente chaque depot :
#
# - "atelier-<composant>" (5 : controller/api-server/dashboard/pm-engine/
#   kvm-device-plugin) : Deployments geres par charts/atelier, pousses
#   depuis les images CI (aujourd'hui ghcr.io/philippevienne/atelier-*).
# - "atelier-<composant>" (5 autres : net-proxy/identity-proxy/
#   vm-supervisor/mcp-gateway/image-builder) : PAS geres par le chart -
#   images injectees directement par crates/controller dans les pods
#   Workshop (voir ATELIER_COMPONENT_IMAGE_REGISTRY, crates/controller/src/
#   reconcile.rs), construites localement (`docker build`), jamais
#   publiees ailleurs.
# - "mirror/<logiciel>" : dependances tierces (Postgres/pgvector, Keycloak,
#   Forgejo, OpenBao, LiteLLM, Redis, RustFS, client `mc`) miroirees depuis
#   leur registre public d'origine, memes noms/tags que
#   charts/atelier/values.yaml.
variable "repository_names" {
  description = "Noms des depots ECR a creer (sans le prefixe de compte/region)."
  type        = list(string)
  default = [
    "atelier-controller",
    "atelier-api-server",
    "atelier-dashboard",
    "atelier-pm-engine",
    "atelier-kvm-device-plugin",
    "atelier-net-proxy",
    "atelier-identity-proxy",
    "atelier-vm-supervisor",
    "atelier-mcp-gateway",
    "atelier-image-builder",
    "mirror/postgres",
    "mirror/keycloak",
    "mirror/forgejo",
    "mirror/openbao",
    "mirror/litellm",
    "mirror/redis",
    "mirror/rustfs",
    "mirror/minio-mc",
  ]
}

variable "untagged_image_expiry_days" {
  description = "Purge des images sans tag (builds intermediaires, anciens \"latest\"/\"dev\" ecrases) apres ce delai. Les images taguees ne sont jamais purgees automatiquement."
  type        = number
  default     = 14
}
