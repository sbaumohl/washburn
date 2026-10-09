provider "vultr" {
  api_key = var.vultr_api_key
}

# ---------------------------------------------------------------- network

resource "vultr_vpc" "washburn" {
  count       = var.enable_vpc ? 1 : 0
  region      = var.region
  description = "${var.project_tag}-vpc"
}

locals {
  vpc_ids = var.enable_vpc ? [vultr_vpc.washburn[0].id] : null

  # Web: public HTTP(S) plus Tailscale's direct-connection port.
  web_rules = {
    http-v4      = { ip_type = "v4", protocol = "tcp", subnet = "0.0.0.0", subnet_size = 0, port = "80" }
    http-v6      = { ip_type = "v6", protocol = "tcp", subnet = "::", subnet_size = 0, port = "80" }
    https-v4     = { ip_type = "v4", protocol = "tcp", subnet = "0.0.0.0", subnet_size = 0, port = "443" }
    https-v6     = { ip_type = "v6", protocol = "tcp", subnet = "::", subnet_size = 0, port = "443" }
    tailscale-v4 = { ip_type = "v4", protocol = "udp", subnet = "0.0.0.0", subnet_size = 0, port = "41641" }
    tailscale-v6 = { ip_type = "v6", protocol = "udp", subnet = "::", subnet_size = 0, port = "41641" }
  }

  # Crawler and data: no public inbound except Tailscale's direct port.
  # No SSH, no HTTP. All management goes over Tailscale.
  private_rules = {
    tailscale-v4 = { ip_type = "v4", protocol = "udp", subnet = "0.0.0.0", subnet_size = 0, port = "41641" }
    tailscale-v6 = { ip_type = "v6", protocol = "udp", subnet = "::", subnet_size = 0, port = "41641" }
  }
}

# ---------------------------------------------------------------- firewall

resource "vultr_firewall_group" "web" {
  description = "${var.project_tag}-web"
}

resource "vultr_firewall_group" "private" {
  description = "${var.project_tag}-private"
}

resource "vultr_firewall_rule" "web" {
  for_each = local.web_rules

  firewall_group_id = vultr_firewall_group.web.id
  ip_type           = each.value.ip_type
  protocol          = each.value.protocol
  subnet            = each.value.subnet
  subnet_size       = each.value.subnet_size
  port              = each.value.port
  notes             = "web ${each.key}"
}

resource "vultr_firewall_rule" "private" {
  for_each = local.private_rules

  firewall_group_id = vultr_firewall_group.private.id
  ip_type           = each.value.ip_type
  protocol          = each.value.protocol
  subnet            = each.value.subnet
  subnet_size       = each.value.subnet_size
  port              = each.value.port
  notes             = "private ${each.key}"
}

# ---------------------------------------------------------------- instances

resource "vultr_instance" "crawler" {
  count = var.counts.crawler

  label    = "crawler-${count.index + 1}"
  hostname = "crawler-${count.index + 1}"
  region   = var.region
  plan     = var.plans.crawler
  os_id    = var.os_id

  ssh_key_ids       = var.ssh_key_ids
  tags              = [var.project_tag, "crawler"]
  enable_ipv6       = true
  backups           = "disabled"
  activation_email  = false
  firewall_group_id = vultr_firewall_group.private.id
  vpc_ids           = local.vpc_ids

  user_data = templatefile("${path.module}/cloud-init/common.yaml.tftpl", {
    role               = "crawler"
    hostname           = "crawler-${count.index + 1}"
    tailscale_auth_key = var.tailscale_auth_key
  })

  # cloud-init runs only on first boot; later template edits must not rebuild the VM.
  lifecycle {
    ignore_changes = [user_data]
  }
}

resource "vultr_instance" "data" {
  label    = "data"
  hostname = "data"
  region   = var.region
  plan     = var.plans.data
  os_id    = var.os_id

  ssh_key_ids       = var.ssh_key_ids
  tags              = [var.project_tag, "data"]
  enable_ipv6       = true
  backups           = "disabled"
  activation_email  = false
  firewall_group_id = vultr_firewall_group.private.id
  vpc_ids           = local.vpc_ids

  user_data = templatefile("${path.module}/cloud-init/common.yaml.tftpl", {
    role               = "data"
    hostname           = "data"
    tailscale_auth_key = var.tailscale_auth_key
  })

  # cloud-init runs only on first boot; later template edits must not rebuild the VM.
  lifecycle {
    ignore_changes = [user_data]
  }
}

resource "vultr_instance" "web" {
  label    = "web"
  hostname = "web"
  region   = var.region
  plan     = var.plans.web
  os_id    = var.os_id

  ssh_key_ids       = var.ssh_key_ids
  tags              = [var.project_tag, "web"]
  enable_ipv6       = true
  backups           = "disabled"
  activation_email  = false
  firewall_group_id = vultr_firewall_group.web.id
  vpc_ids           = local.vpc_ids

  user_data = templatefile("${path.module}/cloud-init/common.yaml.tftpl", {
    role               = "web"
    hostname           = "web"
    tailscale_auth_key = var.tailscale_auth_key
  })

  # cloud-init runs only on first boot; later template edits must not rebuild the VM.
  lifecycle {
    ignore_changes = [user_data]
  }
}

resource "vultr_reserved_ip" "web" {
  count = var.enable_reserved_ip ? 1 : 0

  region      = var.region
  ip_type     = "v4"
  label       = "${var.project_tag}-web"
  instance_id = vultr_instance.web.id
}
