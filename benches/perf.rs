use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use greentic_setup::qa::shared_questions::merge_shared_with_provider_answers;
use greentic_setup::{ProviderFormSpec, collect_shared_questions};
use qa_spec::{FormSpec, QuestionSpec, QuestionType};
use serde_json::{Map as JsonMap, Value, json};

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

fn synth_answers(shared_keys: usize, provider_keys: usize) -> (Value, Value) {
    let mut shared = JsonMap::new();
    let mut provider = JsonMap::new();

    for i in 0..shared_keys {
        shared.insert(format!("shared_{i}"), json!(format!("value_{i}")));
    }
    shared.insert("public_base_url".into(), json!("https://example.com"));

    for i in 0..provider_keys {
        provider.insert(format!("provider_{i}"), json!(i));
    }
    provider.insert("shared_1".into(), json!("override-should-not-win"));

    (Value::Object(shared), Value::Object(provider))
}

fn bench_collect_shared_questions(c: &mut Criterion) {
    let mut group = c.benchmark_group("shared_questions");
    for (providers, questions) in [(25usize, 40usize), (100usize, 80usize)] {
        let dataset = synth_provider_specs(providers, questions);
        group.throughput(Throughput::Elements(providers as u64));
        group.bench_with_input(
            BenchmarkId::new(
                "collect_shared_questions",
                format!("p{providers}_q{questions}"),
            ),
            &dataset,
            |b, data| b.iter(|| collect_shared_questions(data)),
        );
    }
    group.finish();
}

fn bench_merge_shared_with_provider_answers(c: &mut Criterion) {
    let mut group = c.benchmark_group("answers_merge");
    for (shared_keys, provider_keys) in [(250usize, 250usize), (1000usize, 1000usize)] {
        let (shared, provider) = synth_answers(shared_keys, provider_keys);
        group.throughput(Throughput::Elements((shared_keys + provider_keys) as u64));
        group.bench_with_input(
            BenchmarkId::new(
                "merge_shared_with_provider_answers",
                format!("s{shared_keys}_p{provider_keys}"),
            ),
            &(shared, provider),
            |b, (shared, provider)| {
                b.iter(|| merge_shared_with_provider_answers(shared, Some(provider)))
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_collect_shared_questions,
    bench_merge_shared_with_provider_answers
);
criterion_main!(benches);
