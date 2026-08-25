resource "aws_ecr_repository" "this" {
  for_each = toset(var.repository_names)

  name                 = each.value
  image_tag_mutability = "MUTABLE"
  # Compte de test/dev reconstruit a la demande (voir README.md "Trois
  # paliers") : sans ceci, `terraform destroy` echoue des qu'un depot
  # contient des images (mirror-images.sh en aura toujours pousse).
  force_delete = true

  image_scanning_configuration {
    scan_on_push = true
  }

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

resource "aws_ecr_lifecycle_policy" "expire_untagged" {
  for_each = aws_ecr_repository.this

  repository = each.value.name
  policy = jsonencode({
    rules = [
      {
        rulePriority = 1
        description  = "Purge des images sans tag apres ${var.untagged_image_expiry_days} jours"
        selection = {
          tagStatus   = "untagged"
          countType   = "sinceImagePushed"
          countUnit   = "days"
          countNumber = var.untagged_image_expiry_days
        }
        action = {
          type = "expire"
        }
      }
    ]
  })
}
