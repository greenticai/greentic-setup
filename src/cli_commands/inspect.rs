//! Bundle inspection commands: build, list, status.

use anyhow::{Context, Result};

use crate::bundle;
use crate::cli_args::*;
use crate::cli_helpers::{copy_dir_recursive, resolve_bundle_dir};
use crate::cli_i18n::CliI18n;

/// Build a portable bundle archive.
pub fn build(args: BundleBuildArgs, i18n: &CliI18n) -> Result<()> {
    use crate::gtbundle;

    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    let out_str = args.out.to_string_lossy();
    let is_archive = out_str.ends_with(".gtbundle");

    println!("{}", i18n.t("cli.bundle.build.building"));
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.bundle",
            &[&bundle_dir.display().to_string()]
        )
    );
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.build.output",
            &[&args.out.display().to_string()]
        )
    );
    println!(
        "  Format: {}",
        if is_archive {
            "archive (.gtbundle)"
        } else {
            "directory"
        }
    );

    if let Some(ref tenant) = args.tenant {
        println!("{}", i18n.tf("cli.bundle.add.tenant", &[tenant]));
    }

    if args.doctor && !args.skip_doctor {
        println!("\n{}", i18n.t("cli.bundle.build.running_doctor"));
    }

    if is_archive {
        gtbundle::create_gtbundle(&bundle_dir, &args.out)
            .context("failed to create .gtbundle archive")?;
    } else {
        std::fs::create_dir_all(&args.out).context("failed to create output directory")?;
        copy_dir_recursive(&bundle_dir, &args.out, args.only_used_providers)
            .context("failed to copy bundle")?;
    }

    println!(
        "\n{}",
        i18n.tf(
            "cli.bundle.build.success",
            &[&args.out.display().to_string()]
        )
    );

    Ok(())
}

/// List packs in a bundle.
pub fn list(args: BundleListArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    let mut packs = Vec::new();
    let providers_dir = bundle_dir.join("providers");
    let packs_dir = bundle_dir.join("packs");

    let domain_dir = providers_dir.join(&args.domain);
    if domain_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&domain_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "gtpack")
                && let Some(name) = path.file_stem().and_then(|n| n.to_str())
            {
                packs.push((name.to_string(), args.domain.clone()));
            }
        }
    }

    if packs_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&packs_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "gtpack")
                && let Some(name) = path.file_stem().and_then(|n| n.to_str())
            {
                packs.push((name.to_string(), "pack".to_string()));
            }
        }
    }

    if let Some(ref pack_filter) = args.pack {
        packs.retain(|(name, _)| name.contains(pack_filter));
    }

    if args.format == "json" {
        let output = serde_json::json!({
            "bundle": bundle_dir.display().to_string(),
            "domain": args.domain,
            "pack_count": packs.len(),
            "packs": packs.iter().map(|(name, domain)| {
                serde_json::json!({
                    "name": name,
                    "domain": domain,
                })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "{}",
            i18n.tf(
                "cli.bundle.list.bundle",
                &[&bundle_dir.display().to_string()]
            )
        );
        println!("{}", i18n.tf("cli.bundle.list.domain", &[&args.domain]));
        println!(
            "{}",
            i18n.tf("cli.bundle.list.packs_found", &[&packs.len().to_string()])
        );

        for (name, domain) in &packs {
            println!("  - {} ({})", name, domain);
        }
    }

    Ok(())
}

/// Show bundle status.
pub fn status(args: BundleStatusArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    if !bundle_dir.exists() {
        if args.format == "json" {
            println!(r#"{{"exists": false, "path": "{}"}}"#, bundle_dir.display());
        } else {
            println!(
                "{}",
                i18n.tf(
                    "cli.bundle.status.not_found",
                    &[&bundle_dir.display().to_string()]
                )
            );
        }
        return Ok(());
    }

    let is_valid = bundle::is_bundle_root(&bundle_dir);

    let providers_dir = bundle_dir.join("providers");
    let packs_dir = bundle_dir.join("packs");
    let mut pack_count = 0;
    let mut packs = Vec::new();

    if providers_dir.exists() {
        for domain in &[
            "messaging",
            "events",
            "oauth",
            "secrets",
            "mcp",
            "state",
            "other",
        ] {
            let domain_dir = providers_dir.join(domain);
            if domain_dir.exists()
                && let Ok(entries) = std::fs::read_dir(&domain_dir)
            {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "gtpack") {
                        pack_count += 1;
                        if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                            packs.push(format!("providers/{}/{}", domain, name));
                        }
                    }
                }
            }
        }
    }

    if packs_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&packs_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "gtpack") {
                pack_count += 1;
                if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                    packs.push(format!("packs/{}", name));
                }
            }
        }
    }

    let tenants_dir = bundle_dir.join("tenants");
    let mut tenant_count = 0;
    let mut tenants = Vec::new();

    if tenants_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&tenants_dir)
    {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                tenant_count += 1;
                if let Some(name) = entry.file_name().to_str() {
                    tenants.push(name.to_string());
                }
            }
        }
    }

    if args.format == "json" {
        let status = serde_json::json!({
            "exists": true,
            "valid": is_valid,
            "path": bundle_dir.display().to_string(),
            "pack_count": pack_count,
            "packs": packs,
            "tenant_count": tenant_count,
            "tenants": tenants,
        });
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!(
            "{}",
            i18n.tf(
                "cli.bundle.status.bundle_label",
                &[&bundle_dir.display().to_string()]
            )
        );
        let valid_status = if is_valid {
            i18n.t("cli.bundle.status.valid_yes")
        } else {
            i18n.t("cli.bundle.status.valid_no")
        };
        println!("Valid: {}", valid_status);
        println!(
            "{}",
            i18n.tf("cli.bundle.status.packs", &[&pack_count.to_string()])
        );
        for pack in &packs {
            println!("  - {}", pack);
        }
        println!(
            "{}",
            i18n.tf("cli.bundle.status.tenants", &[&tenant_count.to_string()])
        );
        for tenant in &tenants {
            println!("  - {}", tenant);
        }
    }

    Ok(())
}
