# Rendered from washburn.toml by scripts/render-config.py into
# infra/terraform.tfvars.json. See infra/README.md for the full contract.

variable "vultr_api_key" {
  description = "Vultr API key ([vultr].api_key)."
  type        = string
  sensitive   = true
}

variable "region" {
  description = "Vultr region ID, e.g. \"ewr\" ([vultr].region)."
  type        = string
}

variable "os_id" {
  description = "Vultr OS ID, Ubuntu 24.04 LTS x64 is 2284 ([vultr].os_id). Must be an Ubuntu 24.04 image: cloud-init assumes noble."
  type        = number
}

variable "ssh_key_ids" {
  description = "Vultr SSH key IDs already uploaded to the account ([vultr].ssh_key_ids)."
  type        = list(string)
}

variable "plans" {
  description = "Vultr plan ID per role ([vultr.plans])."
  type = object({
    crawler = string
    data    = string
    web     = string
  })
}

variable "counts" {
  description = "Number of VMs per role ([vultr.counts]). The data and web VMs are always singletons."
  type = object({
    crawler = number
  })
}

variable "tailscale_auth_key" {
  description = "Tailscale pre-auth key, reusable and ephemeral ([tailscale].auth_key). Goes into user-data by design."
  type        = string
  sensitive   = true
}

variable "tailnet" {
  description = "Tailnet MagicDNS suffix, e.g. \"tail1234.ts.net\" ([tailscale].tailnet). Used only for output names."
  type        = string
}

variable "public_host" {
  description = "Public site hostname ([domain].public_host). Used only to build the dns_records_needed output."
  type        = string
}

variable "enable_vpc" {
  description = "Create a Vultr VPC and attach all instances to it. Off by default (D11)."
  type        = bool
  default     = false
}

variable "project_tag" {
  description = "Tag and label prefix applied to every resource."
  type        = string
  default     = "washburn"
}

variable "enable_reserved_ip" {
  description = "Allocate a reserved IPv4 for the web VM so the DNS A record survives re-creation."
  type        = bool
  default     = false
}
