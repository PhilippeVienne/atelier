# Les 3 buckets attendus par `s3Storage.buckets.*` (charts/atelier/values.yaml) :
# sessions (archives terminal/VS Code), snapshots (image-builder), et le
# stockage LFS de Forgejo. Le hook Helm `s3-init-job` est idempotent
# ("bucket deja present") : le creer ici via Terraform (chiffrement,
# versioning, blocage acces public) plutot que de laisser `mc mb` le
# creer avec des reglages par defaut est le choix le plus sur pour de
# vraies donnees AWS.
locals {
  buckets = {
    sessions  = "${var.s3_bucket_prefix}-sessions"
    snapshots = "${var.s3_bucket_prefix}-snapshots"
    forgejo   = "${var.s3_bucket_prefix}-forgejo-lfs-attachments"
  }
}

resource "aws_s3_bucket" "this" {
  for_each = local.buckets

  bucket = each.value

  tags = {
    "atelier.dev/cluster" = var.cluster_name
    "atelier.dev/bucket"  = each.key
  }
}

resource "aws_s3_bucket_versioning" "this" {
  for_each = aws_s3_bucket.this

  bucket = each.value.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "this" {
  for_each = aws_s3_bucket.this

  bucket = each.value.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "aws:kms"
    }
    bucket_key_enabled = true
  }
}

resource "aws_s3_bucket_public_access_block" "this" {
  for_each = aws_s3_bucket.this

  bucket                  = each.value.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}
