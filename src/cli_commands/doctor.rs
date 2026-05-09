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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_args::DoctorArgs;
    use crate::cli_i18n::CliI18n;
    use crate::doctor::{Diagnostic, DoctorReport};

    fn args(bundle: std::path::PathBuf) -> DoctorArgs {
        DoctorArgs {
            bundle,
            json: false,
            strict: false,
            fix_hints: false,
            show_info: false,
            stage: None,
        }
    }

    fn i18n() -> CliI18n {
        CliI18n::from_request(Some("en")).expect("english catalog")
    }

    #[test]
    fn doctor_returns_error_for_missing_bundle() {
        let mut args = args(std::path::PathBuf::from(
            "/definitely/missing/greentic-bundle",
        ));
        args.json = true;

        let err = doctor(args, &i18n()).expect_err("missing bundle should fail");
        assert!(err.to_string().contains("doctor found issues"));
    }

    #[test]
    fn doctor_strict_fails_on_stage_warning() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(crate::bundle::BUNDLE_WORKSPACE_MARKER),
            "schema_version: 1\n",
        )
        .unwrap();
        let mut args = args(root);
        args.strict = true;
        args.stage = Some(DoctorStageArg::Routes);

        let err = doctor(args, &i18n()).expect_err("strict mode should fail on route warning");
        assert!(err.to_string().contains("doctor found issues"));
    }

    #[test]
    fn doctor_accepts_clean_runtime_stage() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo");
        crate::bundle::create_demo_bundle_structure(&root, Some("demo")).unwrap();
        std::fs::create_dir_all(root.join("state/runtime")).unwrap();
        let mut args = args(root);
        args.stage = Some(DoctorStageArg::Runtime);

        doctor(args, &i18n()).expect("runtime stage should pass");
    }

    #[test]
    fn human_report_renders_optional_fields() {
        let report = DoctorReport {
            bundle: "demo".to_string(),
            status: "error".to_string(),
            error_count: 1,
            warn_count: 1,
            info_count: 1,
            diagnostics: vec![
                Diagnostic {
                    check_id: "setup.error".to_string(),
                    severity: DiagnosticSeverity::Error,
                    component: "setup".to_string(),
                    message: "broken".to_string(),
                    evidence: Some("evidence".to_string()),
                    expected: Some("expected".to_string()),
                    actual: Some("actual".to_string()),
                    fix_hint: Some("fix it".to_string()),
                    related_file: Some("bundle.yaml".to_string()),
                    related_pack: Some("pack.gtpack".to_string()),
                    related_component: Some("component".to_string()),
                },
                Diagnostic {
                    check_id: "setup.info".to_string(),
                    severity: DiagnosticSeverity::Info,
                    component: "runtime".to_string(),
                    message: "note".to_string(),
                    evidence: None,
                    expected: None,
                    actual: None,
                    fix_hint: None,
                    related_file: None,
                    related_pack: None,
                    related_component: None,
                },
            ],
        };

        print_human_report(&report, true, true);
        print_human_report(&report, false, false);
    }
}
