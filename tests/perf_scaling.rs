use greentic_setup::{ProviderFormSpec, collect_shared_questions};
use qa_spec::{FormSpec, QuestionSpec, QuestionType};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn make_question(id: &str, required: bool, secret: bool) -> QuestionSpec {
    QuestionSpec {
        id: id.to_string(),
        kind: QuestionType::String,
        title: id.to_string(),
        title_i18n: None,
        description: None,
        description_i18n: None,
        required,
        choices: None,
        default_value: None,
        secret,
        visible_if: None,
        constraint: None,
        list: None,
        computed: None,
        policy: Default::default(),
        computed_overridable: false,
    }
}

fn make_provider_form_spec(provider_id: &str, question_ids: &[String]) -> ProviderFormSpec {
    let questions = question_ids
        .iter()
        .map(|id| make_question(id, true, id.contains("token") || id.contains("secret")))
        .collect();
    ProviderFormSpec {
        provider_id: provider_id.to_string(),
        form_spec: FormSpec {
            id: format!("{provider_id}-setup"),
            title: format!("{provider_id} Setup"),
            version: "1.0.0".to_string(),
            description: None,
            presentation: None,
            progress_policy: None,
            secrets_policy: None,
            store: vec![],
            validations: vec![],
            includes: vec![],
            questions,
        },
    }
}

fn synth_provider_specs(provider_count: usize, unique_questions: usize) -> Vec<ProviderFormSpec> {
    let mut providers = Vec::with_capacity(provider_count);
    let mut ids: Vec<String> = vec!["public_base_url".to_string()];
    for i in 0..unique_questions {
        ids.push(format!("q_{i}"));
    }
    ids.push("bot_token".to_string());

    for p in 0..provider_count {
        let mut provider_qs = ids.clone();
        provider_qs.push(format!("provider_{p}_only"));
        providers.push(make_provider_form_spec(
            &format!("messaging-provider-{p}"),
            &provider_qs,
        ));
    }
    providers
}

fn run_workload(
    threads: usize,
    total_tasks: usize,
    providers: Arc<Vec<ProviderFormSpec>>,
) -> Duration {
    let start = Instant::now();
    let base = total_tasks / threads;
    let extra = total_tasks % threads;

    let handles: Vec<_> = (0..threads)
        .map(|idx| {
            let providers = Arc::clone(&providers);
            let tasks = if idx < extra { base + 1 } else { base };
            std::thread::spawn(move || {
                let mut checksum = 0usize;
                for _ in 0..tasks {
                    let shared = collect_shared_questions(&providers);
                    checksum = checksum.wrapping_add(shared.shared_questions.len());
                }
                checksum
            })
        })
        .collect();

    let mut checksum = 0usize;
    for handle in handles {
        checksum = checksum.wrapping_add(handle.join().expect("thread join"));
    }
    assert!(checksum > 0);

    start.elapsed()
}

#[test]
fn scaling_should_not_degrade_badly() {
    let providers = Arc::new(synth_provider_specs(100, 80));
    let total_tasks = 180;

    let t1 = run_workload(1, total_tasks, Arc::clone(&providers));
    let t4 = run_workload(4, total_tasks, Arc::clone(&providers));
    let t8 = run_workload(8, total_tasks, providers);
    println!("scaling timings: t1={t1:?}, t4={t4:?}, t8={t8:?}");

    assert!(
        t4 <= t1.mul_f64(2.2),
        "4 threads slower than expected: t1={:?}, t4={:?}",
        t1,
        t4
    );

    assert!(
        t8 <= t1.mul_f64(3.2),
        "8 threads slower than expected: t1={:?}, t8={:?}",
        t1,
        t8
    );
}
