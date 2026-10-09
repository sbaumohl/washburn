output "web_public_ipv4" {
  description = "Public IPv4 of the web VM (reserved IP if enabled). Target of the DNS A record."
  value       = var.enable_reserved_ip ? vultr_reserved_ip.web[0].subnet : vultr_instance.web.main_ip
}

output "web_public_ipv6" {
  description = "Public IPv6 of the web VM. Target of the DNS AAAA record."
  value       = vultr_instance.web.v6_main_ip
}

output "instance_ids" {
  description = "Vultr instance IDs keyed by role."
  value = {
    crawler = vultr_instance.crawler[*].id
    data    = [vultr_instance.data.id]
    web     = [vultr_instance.web.id]
  }
}

output "hostnames" {
  description = "Vultr hostnames keyed by role."
  value = {
    crawler = vultr_instance.crawler[*].hostname
    data    = [vultr_instance.data.hostname]
    web     = [vultr_instance.web.hostname]
  }
}

output "tailscale_names" {
  description = "Expected MagicDNS names (<hostname>.<tailnet>) keyed by role. Lists, so crawler-N is uniform with data and web."
  value = {
    crawler = [for h in vultr_instance.crawler[*].hostname : "${h}.${var.tailnet}"]
    data    = ["${vultr_instance.data.hostname}.${var.tailnet}"]
    web     = ["${vultr_instance.web.hostname}.${var.tailnet}"]
  }
}

output "dns_records_needed" {
  description = "What the operator must create by hand. DNS is not managed by OpenTofu."
  value = join("", [
    "Create DNS records for ${var.public_host}: A -> ${var.enable_reserved_ip ? vultr_reserved_ip.web[0].subnet : vultr_instance.web.main_ip}",
    " and AAAA -> ${vultr_instance.web.v6_main_ip} (web VM). ",
    "The admin host is Tailscale-only and needs no public record.",
  ])
}
