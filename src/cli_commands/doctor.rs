//! Doctor command rendering for greentic-setup.

use anyhow::Result;

use crate::cli_args::{DoctorArgs, DoctorStageArg};
use crate::cli_i18n::CliI18n;
use crate::doctor::{DiagnosticSeverity, DoctorStage, run_doctor};

pub fn doctor(args: DoctorArgs, _i18n: &CliI18n) -> Result<()> {
    let stage = args.stage.map(|value| match value {
        DoctorStageArg::Setup => DoctorStage::Setup,
        DoctorStageArg::Cache => DoctorStage::Cache,
        DoctorStageArg::Locks => DoctorStage::Locks,
        DoctorStageArg::Answers => DoctorStage::Answers,
        DoctorStageArg::Runtime => DoctorStage::Runtime,
        DoctorStageArg::Routes => DoctorStage::Routes,
    });
    let report = run_doctor(&args.bundle, stage);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report, args.fix_hints, args.show_info);
    }

    if report.error_count > 0 || args.strict && report.warn_count > 0 {
        anyhow::bail!("doctor found issues");
    }
    Ok(())
}

fn print_human_report(report: &crate::doctor::DoctorReport, fix_hints: bool, show_info: bool) {
    println!("greentic-setup doctor");
    println!("bundle: {}", report.bundle);
    println!(
        "status: {} (errors={}, warnings={}, info={})",
        report.status, report.error_count, report.warn_count, report.info_count
    );
    println!();

    for diagnostic in &report.diagnostics {
        if diagnostic.severity == DiagnosticSeverity::Info && !show_info {
            continue;
        }
        let marker = match diagnostic.severity {
            DiagnosticSeverity::Error => "ERROR",
            DiagnosticSeverity::Warn => "WARN",
            DiagnosticSeverity::Info => "INFO",
        };
        println!(
            "[{}] {} {}: {}",
            marker, diagnostic.component, diagnostic.check_id, diagnostic.message
        );
        if let Some(file) = &diagnostic.related_file {
            println!("  file: {file}");
        }
        if let Some(pack) = &diagnostic.related_pack {
            println!("  pack: {pack}");
        }
        if let Some(component) = &diagnostic.related_component {
            println!("  component: {component}");
        }
        if let Some(evidence) = &diagnostic.evidence {
            println!("  evidence: {evidence}");
        }
        if let Some(expected) = &diagnostic.expected {
            println!("  expected: {expected}");
        }
        if let Some(actual) = &diagnostic.actual {
            println!("  actual: {actual}");
        }
        if fix_hints && let Some(hint) = &diagnostic.fix_hint {
            println!("  fix: {hint}");
        }
    }
}
