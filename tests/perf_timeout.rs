use greentic_setup::{ProviderFormSpec, collect_shared_questions};
use qa_spec::{FormSpec, QuestionSpec, QuestionType};
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

#[test]
fn shared_question_collection_should_finish_quickly() {
    let providers = synth_provider_specs(80, 60);
    let start = Instant::now();

    let mut checksum = 0usize;
    for _ in 0..80 {
        checksum =
            checksum.wrapping_add(collect_shared_questions(&providers).shared_questions.len());
    }

    let elapsed = start.elapsed();
    assert!(checksum > 0);
    assert!(
        elapsed < Duration::from_secs(3),
        "shared question workload too slow: {:?}",
        elapsed
    );
}
